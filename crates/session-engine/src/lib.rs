//! Durable session history: SQLite is an adapter, while the event stream is the
//! only source of truth. Projections and effective fork histories are rebuildable.

use harness_protocol::{
    canonical_json, now_ms, ArtifactId, BackendOperationState, EventEnvelope, EventPayload,
    SessionId, ToolAuditResult, ToolIntent, ToolStatus, EVENT_SCHEMA_VERSION, PROTOCOL_NAME,
    PROTOCOL_VERSION,
};
use rusqlite::{
    params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("session `{0}` was not found")]
    SessionNotFound(String),
    #[error("event sequence {sequence} is not present in session `{session_id}`")]
    EventNotFound { session_id: String, sequence: i64 },
    #[error("event hash chain is invalid at session `{session_id}` sequence {sequence}")]
    InvalidHash { session_id: String, sequence: i64 },
    #[error("cannot fork session with pending operation `{0}`")]
    PendingOperation(String),
    #[error("fork lineage is too deep")]
    LineageTooDeep,
    #[error("fork source hash does not match the durable event")]
    ForkAnchorMismatch,
    #[error("invalid session event transition: {0}")]
    InvalidState(String),
    #[error("mutex was poisoned")]
    Poisoned,
    #[error("command idempotency key was reused with different request data")]
    CommandIdempotencyConflict,
    #[error("command `{0}` is still pending")]
    CommandPending(String),
    #[error("expected head does not match current session head (expected {expected_global_sequence}/{expected_hash}, actual {actual_global_sequence}/{actual_hash})")]
    ExpectedHeadMismatch {
        expected_global_sequence: i64,
        expected_hash: String,
        actual_global_sequence: i64,
        actual_hash: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandHead {
    pub global_sequence: i64,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CommandState {
    Pending,
    Committed,
    Rejected,
    Aborted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandReceipt {
    pub command_id: String,
    pub method: String,
    pub request_digest: String,
    pub state: CommandState,
    pub result: Option<Value>,
    pub error: Option<Value>,
    pub event_ids: Vec<String>,
    pub created_at_ms: i64,
    pub completed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactMetadata {
    pub artifact_id: ArtifactId,
    pub session_id: String,
    pub operation_id: String,
    pub kind: String,
    pub sha256: String,
    pub bytes: u64,
    pub media_type: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRecord {
    pub metadata: ArtifactMetadata,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactInput {
    pub kind: String,
    pub content: String,
    pub media_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutboxMessage {
    pub message_id: i64,
    pub subscription_id: String,
    pub event: EventEnvelope,
    pub delivery_attempts: i64,
    pub available_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommandClaim {
    New,
    Existing(CommandReceipt),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PureCommandResult {
    New {
        receipt: CommandReceipt,
        events: Vec<EventEnvelope>,
    },
    Existing(CommandReceipt),
}

#[derive(Clone)]
pub struct SqliteEventStore {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteEventStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SessionError> {
        let connection = Connection::open(path)?;
        configure_connection(&connection)?;
        initialize_schema(&connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn in_memory() -> Result<Self, SessionError> {
        let connection = Connection::open_in_memory()?;
        configure_connection(&connection)?;
        initialize_schema(&connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub(crate) fn append(
        &self,
        session_id: &str,
        payload: EventPayload,
        correlation_id: Option<String>,
        causation_id: Option<String>,
    ) -> Result<EventEnvelope, SessionError> {
        let mut connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        let transaction =
            connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let event = append_in_transaction(
            &transaction,
            session_id,
            payload,
            correlation_id,
            causation_id,
        )?;
        transaction.commit()?;
        Ok(event)
    }

    /// Claim a durable command key before performing a mutation. A pending claim
    /// is a recovery lease: retries never repeat an external side effect.
    pub fn claim_command(
        &self,
        command_id: &str,
        method: &str,
        request_digest: &str,
        session_id: Option<&str>,
        expected_head: Option<&CommandHead>,
    ) -> Result<CommandClaim, SessionError> {
        let mut connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        let transaction =
            connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT command_id, method, request_digest, state, result_json, error_json,
                        event_ids_json, created_at_ms, completed_at_ms
                 FROM command_receipts WHERE command_id = ?1",
                [command_id],
                decode_command_row,
            )
            .optional()?;
        if let Some(receipt) = existing {
            if receipt.method != method || receipt.request_digest != request_digest {
                return Err(SessionError::CommandIdempotencyConflict);
            }
            transaction.commit()?;
            return Ok(CommandClaim::Existing(receipt));
        }
        if let (Some(session_id), Some(expected_head)) = (session_id, expected_head) {
            let actual: Option<(i64, String)> = transaction
                .query_row(
                    "SELECT global_sequence, hash FROM events
                     WHERE session_id = ?1 ORDER BY sequence DESC LIMIT 1",
                    [session_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let (actual_global_sequence, actual_hash) =
                actual.unwrap_or_else(|| (0, String::new()));
            if actual_global_sequence != expected_head.global_sequence
                || actual_hash != expected_head.hash
            {
                return Err(SessionError::ExpectedHeadMismatch {
                    expected_global_sequence: expected_head.global_sequence,
                    expected_hash: expected_head.hash.clone(),
                    actual_global_sequence,
                    actual_hash,
                });
            }
        }
        transaction.execute(
            "INSERT INTO command_receipts
             (command_id, method, request_digest, state, event_ids_json, created_at_ms)
             VALUES (?1, ?2, ?3, 'pending', '[]', ?4)",
            params![command_id, method, request_digest, now_ms()],
        )?;
        transaction.commit()?;
        Ok(CommandClaim::New)
    }

    /// Execute a command whose facts are all known before the transaction starts.
    /// Receipt, CAS check, event append and committed result share one transaction.
    pub fn execute_pure_command<F>(
        &self,
        command_id: &str,
        method: &str,
        request_digest: &str,
        session_id: Option<&str>,
        expected_head: Option<&CommandHead>,
        abort_command_id: Option<&str>,
        stream_id: &str,
        payloads: Vec<(EventPayload, Option<String>, Option<String>)>,
        build_result: F,
    ) -> Result<PureCommandResult, SessionError>
    where
        F: FnOnce(&[EventEnvelope]) -> Value,
    {
        let mut connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        let transaction =
            connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT command_id, method, request_digest, state, result_json, error_json,
                        event_ids_json, created_at_ms, completed_at_ms
                 FROM command_receipts WHERE command_id = ?1",
                [command_id],
                decode_command_row,
            )
            .optional()?;
        if let Some(receipt) = existing {
            if receipt.method != method || receipt.request_digest != request_digest {
                return Err(SessionError::CommandIdempotencyConflict);
            }
            transaction.commit()?;
            return Ok(PureCommandResult::Existing(receipt));
        }
        if let (Some(session_id), Some(expected_head)) = (session_id, expected_head) {
            let actual: Option<(i64, String)> = transaction
                .query_row(
                    "SELECT global_sequence, hash FROM events
                     WHERE session_id = ?1 ORDER BY sequence DESC LIMIT 1",
                    [session_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let (actual_global_sequence, actual_hash) =
                actual.unwrap_or_else(|| (0, String::new()));
            if actual_global_sequence != expected_head.global_sequence
                || actual_hash != expected_head.hash
            {
                return Err(SessionError::ExpectedHeadMismatch {
                    expected_global_sequence: expected_head.global_sequence,
                    expected_hash: expected_head.hash.clone(),
                    actual_global_sequence,
                    actual_hash,
                });
            }
        }
        if let Some(abort_command_id) = abort_command_id {
            if abort_command_id == command_id {
                return Err(SessionError::InvalidState(
                    "a recovery command cannot abort itself".to_string(),
                ));
            }
            let abort_error = serde_json::to_string(&json!({
                "code": -32014,
                "message": "command was explicitly aborted during recovery"
            }))?;
            let changed = transaction.execute(
                "UPDATE command_receipts
                 SET state = 'rejected', error_json = ?2, completed_at_ms = ?3
                 WHERE command_id = ?1 AND state = 'pending'",
                params![abort_command_id, abort_error, now_ms()],
            )?;
            if changed == 0 {
                return Err(SessionError::InvalidState(
                    "target command is not pending".to_string(),
                ));
            }
        }
        transaction.execute(
            "INSERT INTO command_receipts
             (command_id, method, request_digest, state, event_ids_json, created_at_ms)
             VALUES (?1, ?2, ?3, 'pending', '[]', ?4)",
            params![command_id, method, request_digest, now_ms()],
        )?;
        let mut events = Vec::with_capacity(payloads.len());
        for (payload, correlation_id, causation_id) in payloads {
            events.push(append_in_transaction(
                &transaction,
                stream_id,
                payload,
                correlation_id,
                causation_id,
            )?);
        }
        let result = build_result(&events);
        let result_json = serde_json::to_string(&result)?;
        let event_ids = events
            .iter()
            .map(|event| event.event_id.clone())
            .collect::<Vec<_>>();
        let event_ids_json = serde_json::to_string(&event_ids)?;
        transaction.execute(
            "UPDATE command_receipts
             SET state = 'committed', result_json = ?2, event_ids_json = ?3,
                 completed_at_ms = ?4
             WHERE command_id = ?1 AND state = 'pending'",
            params![command_id, result_json, event_ids_json, now_ms()],
        )?;
        let receipt = transaction.query_row(
            "SELECT command_id, method, request_digest, state, result_json, error_json,
                    event_ids_json, created_at_ms, completed_at_ms
             FROM command_receipts WHERE command_id = ?1",
            [command_id],
            decode_command_row,
        )?;
        transaction.commit()?;
        Ok(PureCommandResult::New { receipt, events })
    }

    pub fn complete_command(
        &self,
        command_id: &str,
        result: Value,
        event_ids: &[String],
    ) -> Result<CommandReceipt, SessionError> {
        self.finish_command(command_id, "committed", Some(result), None, event_ids)
    }

    pub fn reject_command(
        &self,
        command_id: &str,
        error: Value,
    ) -> Result<CommandReceipt, SessionError> {
        self.finish_command(command_id, "rejected", None, Some(error), &[])
    }

    fn finish_command(
        &self,
        command_id: &str,
        state: &str,
        result: Option<Value>,
        error: Option<Value>,
        event_ids: &[String],
    ) -> Result<CommandReceipt, SessionError> {
        let mut connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        let transaction =
            connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result_json = result
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let error_json = error.as_ref().map(serde_json::to_string).transpose()?;
        let event_ids_json = serde_json::to_string(event_ids)?;
        let changed = transaction.execute(
            "UPDATE command_receipts
             SET state = ?2, result_json = ?3, error_json = ?4,
                 event_ids_json = ?5, completed_at_ms = ?6
             WHERE command_id = ?1 AND state = 'pending'",
            params![
                command_id,
                state,
                result_json,
                error_json,
                event_ids_json,
                now_ms()
            ],
        )?;
        if changed == 0 {
            let existing = transaction
                .query_row(
                    "SELECT command_id, method, request_digest, state, result_json, error_json,
                            event_ids_json, created_at_ms, completed_at_ms
                     FROM command_receipts WHERE command_id = ?1",
                    [command_id],
                    decode_command_row,
                )
                .optional()?;
            transaction.commit()?;
            return existing.ok_or_else(|| SessionError::InvalidState(
                "command receipt does not exist".to_string(),
            ));
        }
        let receipt = transaction.query_row(
            "SELECT command_id, method, request_digest, state, result_json, error_json,
                    event_ids_json, created_at_ms, completed_at_ms
             FROM command_receipts WHERE command_id = ?1",
            [command_id],
            decode_command_row,
        )?;
        transaction.commit()?;
        Ok(receipt)
    }

    pub fn direct_events(&self, session_id: &str) -> Result<Vec<EventEnvelope>, SessionError> {
        let connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT protocol, version, schema_version, event_id, session_id, sequence,
                    global_sequence, recorded_at_ms, correlation_id, causation_id,
                    prev_hash, hash, event_type, payload_json
             FROM events WHERE session_id = ?1 ORDER BY sequence ASC",
        )?;
        let rows = statement.query_map([session_id], decode_event_row)?;
        let events: Result<Vec<_>, _> = rows.collect();
        let events = events?;
        if events.is_empty() {
            return Err(SessionError::SessionNotFound(session_id.to_string()));
        }
        verify_hash_chain(session_id, &events)?;
        Ok(events)
    }

    pub fn global_events_after(
        &self,
        after_global_sequence: i64,
        limit: usize,
    ) -> Result<Vec<EventEnvelope>, SessionError> {
        let connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT protocol, version, schema_version, event_id, session_id, sequence,
                    global_sequence, recorded_at_ms, correlation_id, causation_id,
                    prev_hash, hash, event_type, payload_json
             FROM events WHERE global_sequence > ?1
             ORDER BY global_sequence ASC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![after_global_sequence, limit as i64],
            decode_event_row,
        )?;
        let events: Result<Vec<_>, _> = rows.collect();
        let events = events?;
        let mut previous = after_global_sequence;
        for event in &events {
            if event.global_sequence <= previous {
                return Err(SessionError::InvalidState(
                    "global event cursor is not strictly increasing".to_string(),
                ));
            }
            previous = event.global_sequence;
        }
        Ok(events)
    }

    pub fn create_subscription(
        &self,
        session_id: &str,
        after_global_sequence: i64,
    ) -> Result<String, SessionError> {
        let subscription_id = Uuid::new_v4().to_string();
        let connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        connection.execute(
            "INSERT INTO subscriptions
             (subscription_id, session_id, after_global_sequence, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![subscription_id, session_id, after_global_sequence, now_ms()],
        )?;
        Ok(subscription_id)
    }

    pub fn subscription_session_and_cursor(
        &self,
        subscription_id: &str,
    ) -> Result<Option<(String, i64)>, SessionError> {
        let connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        Ok(connection
            .query_row(
                "SELECT session_id, after_global_sequence FROM subscriptions
                 WHERE subscription_id = ?1",
                [subscription_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    pub fn acknowledge_subscription(
        &self,
        subscription_id: &str,
        after_global_sequence: i64,
    ) -> Result<(), SessionError> {
        let mut connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        let transaction =
            connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<i64> = transaction
            .query_row(
                "SELECT after_global_sequence FROM subscriptions WHERE subscription_id = ?1",
                [subscription_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(current) = current else {
            return Err(SessionError::InvalidState(
                "subscription does not exist".to_string(),
            ));
        };
        if after_global_sequence < current {
            return Err(SessionError::InvalidState(
                "subscription cursor cannot move backwards".to_string(),
            ));
        }
        let max_known: Option<i64> = transaction
            .query_row(
                "SELECT MAX(global_sequence) FROM subscription_outbox WHERE subscription_id = ?1",
                [subscription_id],
                |row| row.get(0),
            )?;
        if after_global_sequence > max_known.unwrap_or(current) {
            return Err(SessionError::InvalidState(
                "subscription acknowledgement is ahead of the durable outbox".to_string(),
            ));
        }
        let pending_gap: Option<i64> = transaction
            .query_row(
                "SELECT global_sequence FROM subscription_outbox
                 WHERE subscription_id = ?1 AND global_sequence <= ?2 AND state = 'pending'
                 ORDER BY global_sequence ASC LIMIT 1",
                params![subscription_id, after_global_sequence],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(global_sequence) = pending_gap {
            return Err(SessionError::InvalidState(format!(
                "subscription acknowledgement skips pending global sequence {global_sequence}"
            )));
        }
        transaction.execute(
            "UPDATE subscriptions
             SET after_global_sequence = ?2, updated_at_ms = ?3
             WHERE subscription_id = ?1",
            params![subscription_id, after_global_sequence, now_ms()],
        )?;
        transaction.execute(
            "UPDATE subscription_outbox
             SET state = 'acked', lease_until_ms = NULL, updated_at_ms = ?3
             WHERE subscription_id = ?1 AND global_sequence <= ?2",
            params![subscription_id, after_global_sequence, now_ms()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_subscription(&self, subscription_id: &str) -> Result<bool, SessionError> {
        let mut connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM subscription_outbox WHERE subscription_id = ?1",
            [subscription_id],
        )?;
        let deleted = transaction.execute(
            "DELETE FROM subscriptions WHERE subscription_id = ?1",
            [subscription_id],
        )?;
        transaction.commit()?;
        Ok(deleted > 0)
    }

    pub fn enqueue_subscription_events(
        &self,
        subscription_id: &str,
        events: &[EventEnvelope],
    ) -> Result<(), SessionError> {
        let mut connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        let transaction =
            connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let subscription_exists: Option<i64> = transaction
            .query_row(
                "SELECT 1 FROM subscriptions WHERE subscription_id = ?1",
                [subscription_id],
                |row| row.get(0),
            )
            .optional()?;
        if subscription_exists.is_none() {
            return Err(SessionError::InvalidState(
                "subscription does not exist".to_string(),
            ));
        }
        for event in events {
            let event_json = serde_json::to_string(event)?;
            transaction.execute(
                "INSERT OR IGNORE INTO subscription_outbox
                 (subscription_id, event_id, session_id, global_sequence, event_json,
                  state, delivery_attempts, available_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 0, ?6, ?6)",
                params![
                    subscription_id,
                    &event.event_id,
                    &event.session_id,
                    event.global_sequence,
                    event_json,
                    now_ms()
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn claim_outbox(
        &self,
        subscription_id: &str,
        limit: usize,
        lease_ms: i64,
    ) -> Result<Vec<OutboxMessage>, SessionError> {
        if limit == 0 || limit > 1000 || lease_ms <= 0 {
            return Err(SessionError::InvalidState(
                "outbox limit must be between 1 and 1000 and lease_ms must be positive".to_string(),
            ));
        }
        let mut connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        let transaction =
            connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_ms();
        let mut statement = transaction.prepare(
            "SELECT message_id, event_json, delivery_attempts, available_at_ms
             FROM subscription_outbox
             WHERE subscription_id = ?1
               AND (state = 'pending' OR (state = 'in_flight' AND lease_until_ms < ?2))
               AND available_at_ms <= ?2
             ORDER BY global_sequence ASC, message_id ASC LIMIT ?3",
        )?;
        let rows = statement.query_map(params![subscription_id, now, limit as i64], |row| {
            let event_json: String = row.get(1)?;
            let event: EventEnvelope = serde_json::from_str(&event_json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok((row.get::<_, i64>(0)?, event, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?))
        })?;
        let rows: Result<Vec<_>, _> = rows.collect();
        drop(statement);
        let rows = rows?;
        let lease_until = now.saturating_add(lease_ms);
        let mut messages = Vec::with_capacity(rows.len());
        for (message_id, event, attempts, available_at_ms) in rows {
            transaction.execute(
                "UPDATE subscription_outbox
                 SET state = 'in_flight', delivery_attempts = delivery_attempts + 1,
                     lease_until_ms = ?2, updated_at_ms = ?3
                 WHERE message_id = ?1 AND subscription_id = ?4
                   AND (state = 'pending' OR (state = 'in_flight' AND lease_until_ms < ?3))",
                params![message_id, lease_until, now, subscription_id],
            )?;
            messages.push(OutboxMessage {
                message_id,
                subscription_id: subscription_id.to_string(),
                event,
                delivery_attempts: attempts + 1,
                available_at_ms,
            });
        }
        transaction.commit()?;
        Ok(messages)
    }

    pub fn nack_outbox(
        &self,
        subscription_id: &str,
        message_id: i64,
        error: &str,
        retry_after_ms: i64,
    ) -> Result<(), SessionError> {
        let connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        let changed = connection.execute(
            "UPDATE subscription_outbox
             SET state = 'pending', available_at_ms = ?3, lease_until_ms = NULL,
                 last_error = ?4, updated_at_ms = ?3
             WHERE message_id = ?1 AND subscription_id = ?2 AND state = 'in_flight'",
            params![message_id, subscription_id, now_ms().saturating_add(retry_after_ms.max(0)), error],
        )?;
        if changed == 0 {
            return Err(SessionError::InvalidState(
                "outbox message is not leased for this subscription".to_string(),
            ));
        }
        Ok(())
    }

    pub fn put_artifact(
        &self,
        session_id: &str,
        operation_id: &str,
        kind: &str,
        content: &str,
        media_type: &str,
    ) -> Result<ArtifactMetadata, SessionError> {
        let artifact_id = Uuid::new_v4().to_string();
        let digest = Sha256::digest(content.as_bytes());
        let metadata = ArtifactMetadata {
            artifact_id: artifact_id.clone(),
            session_id: session_id.to_string(),
            operation_id: operation_id.to_string(),
            kind: kind.to_string(),
            sha256: format!("sha256:{digest:x}"),
            bytes: content.as_bytes().len() as u64,
            media_type: media_type.to_string(),
            created_at_ms: now_ms(),
        };
        let connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        connection.execute(
            "INSERT INTO artifacts
             (artifact_id, session_id, operation_id, kind, sha256, bytes, media_type,
              content, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                &metadata.artifact_id,
                &metadata.session_id,
                &metadata.operation_id,
                &metadata.kind,
                &metadata.sha256,
                metadata.bytes as i64,
                &metadata.media_type,
                content,
                metadata.created_at_ms
            ],
        )?;
        Ok(metadata)
    }

    pub fn finish_operation(
        &self,
        session_id: &str,
        operation_id: &str,
        turn_id: &str,
        run_id: &str,
        mut audit: ToolAuditResult,
        artifacts: Vec<ArtifactInput>,
        complete_run: bool,
    ) -> Result<(Vec<EventEnvelope>, ToolAuditResult), SessionError> {
        let mut connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        let transaction =
            connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut events = Vec::new();
        for input in artifacts {
            let artifact_id = Uuid::new_v4().to_string();
            let digest = Sha256::digest(input.content.as_bytes());
            let sha256 = format!("sha256:{digest:x}");
            let bytes = input.content.as_bytes().len() as u64;
            let created_at_ms = now_ms();
            let kind = input.kind.clone();
            let media_type = input.media_type.clone();
            transaction.execute(
                "INSERT INTO artifacts
                 (artifact_id, session_id, operation_id, kind, sha256, bytes, media_type,
                  content, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    &artifact_id,
                    session_id,
                    operation_id,
                    &kind,
                    &sha256,
                    bytes as i64,
                    &media_type,
                    &input.content,
                    created_at_ms
                ],
            )?;
            if kind == "stdout" {
                audit.stdout_artifact_id = Some(artifact_id.clone());
            } else if kind == "stderr" {
                audit.stderr_artifact_id = Some(artifact_id.clone());
            }
            events.push(append_in_transaction(
                &transaction,
                session_id,
                EventPayload::ArtifactCreated {
                    artifact_id,
                    operation_id: operation_id.to_string(),
                    kind,
                    sha256,
                    bytes,
                    media_type,
                },
                Some(turn_id.to_string()),
                None,
            )?);
        }
        events.push(append_in_transaction(
            &transaction,
            session_id,
            EventPayload::ToolFinished {
                operation_id: operation_id.to_string(),
                result: audit.clone(),
            },
            Some(turn_id.to_string()),
            None,
        )?);
        if complete_run {
            events.push(append_in_transaction(
                &transaction,
                session_id,
                EventPayload::RunCompleted {
                    run_id: run_id.to_string(),
                    success: matches!(&audit.status, ToolStatus::Succeeded),
                },
                Some(turn_id.to_string()),
                None,
            )?);
        }
        transaction.commit()?;
        Ok((events, audit))
    }

    pub fn get_artifact(
        &self,
        session_id: &str,
        artifact_id: &str,
    ) -> Result<Option<ArtifactRecord>, SessionError> {
        let connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        let record: Option<ArtifactRecord> = connection
            .query_row(
                "SELECT artifact_id, session_id, operation_id, kind, sha256, bytes,
                        media_type, content, created_at_ms
                 FROM artifacts WHERE artifact_id = ?1 AND session_id = ?2",
                params![artifact_id, session_id],
                |row| {
                    Ok(ArtifactRecord {
                        metadata: ArtifactMetadata {
                            artifact_id: row.get(0)?,
                            session_id: row.get(1)?,
                            operation_id: row.get(2)?,
                            kind: row.get(3)?,
                            sha256: row.get(4)?,
                            bytes: row.get::<_, i64>(5)? as u64,
                            media_type: row.get(6)?,
                            created_at_ms: row.get(8)?,
                        },
                        content: row.get(7)?,
                    })
                },
            )
            .optional()?;
        if let Some(record) = &record {
            let digest = Sha256::digest(record.content.as_bytes());
            let expected = format!("sha256:{digest:x}");
            if record.metadata.sha256.as_str() != expected.as_str()
                || record.metadata.bytes != record.content.as_bytes().len() as u64
            {
                return Err(SessionError::InvalidState(
                    "artifact digest or byte count does not match durable content".to_string(),
                ));
            }
        }
        Ok(record)
    }

    pub fn list_artifacts(&self, session_id: &str) -> Result<Vec<ArtifactMetadata>, SessionError> {
        let connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT artifact_id, session_id, operation_id, kind, sha256, bytes,
                    media_type, created_at_ms
             FROM artifacts
             WHERE session_id = ?1
             ORDER BY created_at_ms ASC, rowid ASC",
        )?;
        let artifacts = statement
            .query_map([session_id], |row| {
                Ok(ArtifactMetadata {
                    artifact_id: row.get(0)?,
                    session_id: row.get(1)?,
                    operation_id: row.get(2)?,
                    kind: row.get(3)?,
                    sha256: row.get(4)?,
                    bytes: row.get::<_, i64>(5)? as u64,
                    media_type: row.get(6)?,
                    created_at_ms: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(artifacts)
    }

    pub fn has_session(&self, session_id: &str) -> Result<bool, SessionError> {
        let connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
        let exists: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM events WHERE session_id = ?1 LIMIT 1",
                [session_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(exists.is_some())
    }

    pub fn rebuild_check(&self) -> Result<(), SessionError> {
        let ids = {
            let connection = self.connection.lock().map_err(|_| SessionError::Poisoned)?;
            let mut sessions = connection.prepare("SELECT DISTINCT session_id FROM events")?;
            let ids: Result<Vec<String>, _> = sessions
                .query_map([], |row| row.get(0))?
                .collect();
            ids?
        };
        for session_id in ids {
            let events = self.direct_events(&session_id)?;
            verify_hash_chain(&session_id, &events)?;
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct SessionEngine {
    store: SqliteEventStore,
    append_lock: Arc<Mutex<()>>,
}

impl SessionEngine {
    pub fn new(store: SqliteEventStore) -> Self {
        Self {
            store,
            append_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn store(&self) -> &SqliteEventStore {
        &self.store
    }

    pub fn claim_command(
        &self,
        command_id: &str,
        method: &str,
        request_digest: &str,
        session_id: Option<&str>,
        expected_head: Option<&CommandHead>,
    ) -> Result<CommandClaim, SessionError> {
        self.store.claim_command(
            command_id,
            method,
            request_digest,
            session_id,
            expected_head,
        )
    }

    pub fn execute_pure_command<F>(
        &self,
        command_id: &str,
        method: &str,
        request_digest: &str,
        session_id: Option<&str>,
        expected_head: Option<&CommandHead>,
        abort_command_id: Option<&str>,
        stream_id: &str,
        payloads: Vec<(EventPayload, Option<String>, Option<String>)>,
        build_result: F,
    ) -> Result<PureCommandResult, SessionError>
    where
        F: FnOnce(&[EventEnvelope]) -> Value,
    {
        self.store.execute_pure_command(
            command_id,
            method,
            request_digest,
            session_id,
            expected_head,
            abort_command_id,
            stream_id,
            payloads,
            build_result,
        )
    }

    pub fn complete_command(
        &self,
        command_id: &str,
        result: Value,
        event_ids: &[String],
    ) -> Result<CommandReceipt, SessionError> {
        self.store.complete_command(command_id, result, event_ids)
    }

    pub fn reject_command(
        &self,
        command_id: &str,
        error: Value,
    ) -> Result<CommandReceipt, SessionError> {
        self.store.reject_command(command_id, error)
    }

    pub fn finish_operation(
        &self,
        session_id: &str,
        operation_id: &str,
        turn_id: &str,
        run_id: &str,
        audit: ToolAuditResult,
        artifacts: Vec<ArtifactInput>,
        complete_run: bool,
    ) -> Result<(Vec<EventEnvelope>, ToolAuditResult), SessionError> {
        let _append_guard = self
            .append_lock
            .lock()
            .map_err(|_| SessionError::Poisoned)?;
        let projection = self.replay(session_id)?;
        let operation = projection
            .operations
            .get(operation_id)
            .ok_or_else(|| SessionError::InvalidState("operation does not exist".to_string()))?;
        if !matches!(&operation.status, OperationStatus::Started) {
            return Err(SessionError::InvalidState(
                "operation is not started or has already been finalized".to_string(),
            ));
        }
        if operation.turn_id != turn_id || operation.run_id != run_id {
            return Err(SessionError::InvalidState(
                "operation correlation does not match the active turn".to_string(),
            ));
        }
        self.store.finish_operation(
            session_id,
            operation_id,
            turn_id,
            run_id,
            audit,
            artifacts,
            complete_run,
        )
    }

    pub fn create_session(
        &self,
        workspace_roots: Vec<String>,
        model: Option<String>,
    ) -> Result<(SessionId, EventEnvelope), SessionError> {
        let session_id = Uuid::new_v4().to_string();
        let event = self.store.append(
            &session_id,
            EventPayload::SessionCreated {
                workspace_roots,
                model,
            },
            None,
            None,
        )?;
        Ok((session_id, event))
    }

    pub fn create_session_command(
        &self,
        command_id: &str,
        request_digest: &str,
        workspace_roots: Vec<String>,
        model: Option<String>,
    ) -> Result<PureCommandResult, SessionError> {
        let session_id = Uuid::new_v4().to_string();
        let result_session_id = session_id.clone();
        self.store.execute_pure_command(
            command_id,
            "runtime.v1.session.create",
            request_digest,
            None,
            None,
            None,
            &session_id,
            vec![(
                EventPayload::SessionCreated {
                    workspace_roots,
                    model,
                },
                None,
                None,
            )],
            move |events| {
                json!({
                    "session_id": result_session_id,
                    "event": events.first().expect("create emits one event")
                })
            },
        )
    }

    pub fn append(
        &self,
        session_id: &str,
        payload: EventPayload,
        correlation_id: Option<String>,
        causation_id: Option<String>,
    ) -> Result<EventEnvelope, SessionError> {
        let _append_guard = self
            .append_lock
            .lock()
            .map_err(|_| SessionError::Poisoned)?;
        let has_session = self.store.has_session(session_id)?;
        if !has_session && !matches!(&payload, EventPayload::SessionCreated { .. }) {
            return Err(SessionError::SessionNotFound(session_id.to_string()));
        }
        if has_session {
            let projection = self.replay(session_id)?;
            validate_transition(&projection, &payload)?;
        }
        self.store
            .append(session_id, payload, correlation_id, causation_id)
    }

    /// Returns the effective branch history: parent prefix followed by the child
    /// branch's own events. The fork marker stays in the effective log so lineage
    /// is observable, but source events are never copied into the child stream.
    pub fn effective_events(&self, session_id: &str) -> Result<Vec<EventEnvelope>, SessionError> {
        self.effective_events_at(session_id, None, 0)
    }

    pub fn replay(&self, session_id: &str) -> Result<SessionProjection, SessionError> {
        let events = self.effective_events(session_id)?;
        SessionProjection::from_events(session_id.to_string(), &events)
    }

    pub fn subscribe(
        &self,
        session_id: &str,
        after_global_sequence: i64,
        limit: usize,
    ) -> Result<SubscriptionView, SessionError> {
        if limit == 0 || limit > 1000 {
            return Err(SessionError::InvalidState(
                "subscription limit must be between 1 and 1000".to_string(),
            ));
        }
        let subscription_id = self
            .store
            .create_subscription(session_id, after_global_sequence)?;
        let backlog = self
            .effective_events(session_id)?
            .into_iter()
            .filter(|event| event.global_sequence > after_global_sequence)
            .collect::<Vec<_>>();
        self.store
            .enqueue_subscription_events(&subscription_id, &backlog)?;
        self.subscription_backlog(&subscription_id, limit)
    }

    pub fn subscription_backlog(
        &self,
        subscription_id: &str,
        limit: usize,
    ) -> Result<SubscriptionView, SessionError> {
        if limit == 0 || limit > 1000 {
            return Err(SessionError::InvalidState(
                "subscription limit must be between 1 and 1000".to_string(),
            ));
        }
        let Some((session_id, after_global_sequence)) = self
            .store
            .subscription_session_and_cursor(subscription_id)?
        else {
            return Err(SessionError::InvalidState(
                "subscription does not exist".to_string(),
            ));
        };
        let messages = self.store.claim_outbox(subscription_id, limit, 60_000)?;
        let events = messages
            .iter()
            .map(|message| message.event.clone())
            .collect::<Vec<_>>();
        let has_more = messages.len() == limit;
        let next_global_sequence = events
            .last()
            .map(|event| event.global_sequence)
            .unwrap_or(after_global_sequence);
        Ok(SubscriptionView {
            subscription_id: subscription_id.to_string(),
            session_id,
            after_global_sequence,
            events,
            deliveries: messages,
            next_global_sequence,
            has_more,
        })
    }

    pub fn claim_outbox(
        &self,
        subscription_id: &str,
        limit: usize,
        lease_ms: i64,
    ) -> Result<Vec<OutboxMessage>, SessionError> {
        self.store.claim_outbox(subscription_id, limit, lease_ms)
    }

    pub fn nack_outbox(
        &self,
        subscription_id: &str,
        message_id: i64,
        error: &str,
        retry_after_ms: i64,
    ) -> Result<(), SessionError> {
        self.store
            .nack_outbox(subscription_id, message_id, error, retry_after_ms)
    }

    pub fn put_artifact(
        &self,
        session_id: &str,
        operation_id: &str,
        kind: &str,
        content: &str,
        media_type: &str,
    ) -> Result<ArtifactMetadata, SessionError> {
        if !self.store.has_session(session_id)? {
            return Err(SessionError::SessionNotFound(session_id.to_string()));
        }
        self.store
            .put_artifact(session_id, operation_id, kind, content, media_type)
    }

    pub fn get_artifact(
        &self,
        session_id: &str,
        artifact_id: &str,
    ) -> Result<Option<ArtifactRecord>, SessionError> {
        self.store.get_artifact(session_id, artifact_id)
    }

    pub fn list_artifacts(&self, session_id: &str) -> Result<Vec<ArtifactMetadata>, SessionError> {
        self.store.list_artifacts(session_id)
    }

    pub fn acknowledge_subscription(
        &self,
        subscription_id: &str,
        after_global_sequence: i64,
    ) -> Result<(), SessionError> {
        self.store
            .acknowledge_subscription(subscription_id, after_global_sequence)
    }

    pub fn delete_subscription(&self, subscription_id: &str) -> Result<bool, SessionError> {
        self.store.delete_subscription(subscription_id)
    }

    pub fn resume(&self, session_id: &str) -> Result<ResumeView, SessionError> {
        let events = self.effective_events(session_id)?;
        let projection = SessionProjection::from_events(session_id.to_string(), &events)?;
        let recovery_required = projection
            .operations
            .values()
            .filter(|operation| !operation.is_terminal())
            .map(|operation| operation.operation_id.clone())
            .collect();
        let running_turn_ids = projection
            .turns
            .values()
            .filter(|turn| turn.status == "running")
            .map(|turn| turn.turn_id.clone())
            .collect();
        let mut model_requests = BTreeMap::new();
        for event in events {
            match event.payload {
                EventPayload::ModelRequested { request_id, .. } => {
                    model_requests.insert(request_id, false);
                }
                EventPayload::ModelResponded { request_id, .. } => {
                    model_requests.insert(request_id, true);
                }
                _ => {}
            }
        }
        let pending_model_requests = model_requests
            .into_iter()
            .filter_map(|(request_id, completed)| (!completed).then_some(request_id))
            .collect();
        Ok(ResumeView {
            latest_global_sequence: projection.as_of_global_sequence,
            projection,
            recovery_required,
            running_turn_ids,
            pending_model_requests,
        })
    }

    pub fn inspect_operation(
        &self,
        session_id: &str,
        operation_id: &str,
    ) -> Result<Option<OperationProjection>, SessionError> {
        let projection = self.replay(session_id)?;
        Ok(projection.operations.get(operation_id).cloned())
    }

    pub fn run_has_pending_operations(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<bool, SessionError> {
        let projection = self.replay(session_id)?;
        Ok(projection
            .operations
            .values()
            .any(|operation| operation.run_id == run_id && !operation.is_terminal()))
    }

    pub fn find_tool_proposal(
        &self,
        session_id: &str,
        operation_id: &str,
    ) -> Result<Option<(String, String, String, ToolIntent)>, SessionError> {
        for event in self.effective_events(session_id)? {
            if let EventPayload::ToolProposed {
                turn_id,
                run_id,
                operation_id: proposed_id,
                tool_name,
                intent,
            } = event.payload
            {
                if proposed_id == operation_id {
                    return Ok(Some((turn_id, run_id, tool_name, intent)));
                }
            }
        }
        Ok(None)
    }

    pub fn fork(
        &self,
        source_session_id: &str,
        source_sequence: i64,
    ) -> Result<(SessionId, EventEnvelope), SessionError> {
        let _append_guard = self
            .append_lock
            .lock()
            .map_err(|_| SessionError::Poisoned)?;
        let source_direct = self.store.direct_events(source_session_id)?;
        let anchor = source_direct
            .iter()
            .find(|event| event.sequence == source_sequence)
            .ok_or_else(|| SessionError::EventNotFound {
                session_id: source_session_id.to_string(),
                sequence: source_sequence,
            })?;
        let source_prefix = self.effective_events_at(source_session_id, Some(source_sequence), 0)?;
        let source_projection = SessionProjection::from_events(
            source_session_id.to_string(),
            &source_prefix,
        )?;
        if source_projection.status == SessionStatus::Completed {
            return Err(SessionError::InvalidState(
                "cannot fork from a completed session anchor".to_string(),
            ));
        }
        if let Some(operation) = source_projection
            .operations
            .values()
            .find(|operation| !operation.is_terminal())
        {
            return Err(SessionError::PendingOperation(operation.operation_id.clone()));
        }

        let child_session_id = Uuid::new_v4().to_string();
        let event = self.store.append(
            &child_session_id,
            EventPayload::SessionForked {
                source_session_id: source_session_id.to_string(),
                source_sequence,
                source_hash: anchor.hash.clone(),
            },
            None,
            Some(anchor.event_id.clone()),
        )?;
        Ok((child_session_id, event))
    }

    pub fn fork_session_command(
        &self,
        command_id: &str,
        request_digest: &str,
        source_session_id: &str,
        source_sequence: i64,
        expected_head: Option<CommandHead>,
    ) -> Result<PureCommandResult, SessionError> {
        let _append_guard = self
            .append_lock
            .lock()
            .map_err(|_| SessionError::Poisoned)?;
        let source_direct = self.store.direct_events(source_session_id)?;
        let anchor = source_direct
            .iter()
            .find(|event| event.sequence == source_sequence)
            .ok_or_else(|| SessionError::EventNotFound {
                session_id: source_session_id.to_string(),
                sequence: source_sequence,
            })?;
        let source_prefix = self.effective_events_at(source_session_id, Some(source_sequence), 0)?;
        let source_projection = SessionProjection::from_events(
            source_session_id.to_string(),
            &source_prefix,
        )?;
        if source_projection.status == SessionStatus::Completed {
            return Err(SessionError::InvalidState(
                "cannot fork from a completed session anchor".to_string(),
            ));
        }
        if let Some(operation) = source_projection
            .operations
            .values()
            .find(|operation| !operation.is_terminal())
        {
            return Err(SessionError::PendingOperation(operation.operation_id.clone()));
        }
        let source_head = source_direct
            .last()
            .map(|event| CommandHead {
                global_sequence: event.global_sequence,
                hash: event.hash.clone(),
            })
            .ok_or_else(|| SessionError::SessionNotFound(source_session_id.to_string()))?;
        let expected_head = expected_head.or(Some(source_head));
        let child_session_id = Uuid::new_v4().to_string();
        let result_session_id = child_session_id.clone();
        self.store.execute_pure_command(
            command_id,
            "runtime.v1.session.fork",
            request_digest,
            Some(source_session_id),
            expected_head.as_ref(),
            None,
            &child_session_id,
            vec![(
                EventPayload::SessionForked {
                    source_session_id: source_session_id.to_string(),
                    source_sequence,
                    source_hash: anchor.hash.clone(),
                },
                None,
                Some(anchor.event_id.clone()),
            )],
            move |events| {
                json!({
                    "session_id": result_session_id,
                    "event": events.first().expect("fork emits one event")
                })
            },
        )
    }

    pub fn recover_command(
        &self,
        command_id: &str,
        request_digest: &str,
        session_id: &str,
        operation_id: Option<String>,
        reason: String,
        expected_head: Option<CommandHead>,
        abort_command_id: Option<&str>,
    ) -> Result<PureCommandResult, SessionError> {
        let _append_guard = self
            .append_lock
            .lock()
            .map_err(|_| SessionError::Poisoned)?;
        let projection = self.replay(session_id)?;
        let mut payloads = vec![(
            EventPayload::RecoveryRequired {
                operation_id: operation_id.clone(),
                reason: reason.clone(),
            },
            None,
            None,
        )];
        if let Some(operation_id) = operation_id {
            let operation = projection
                .operations
                .get(&operation_id)
                .ok_or_else(|| SessionError::InvalidState("operation does not exist".to_string()))?;
            if operation.is_terminal() {
                return Err(SessionError::InvalidState(
                    "operation is already terminal".to_string(),
                ));
            }
            payloads.push((
                EventPayload::ToolFailed {
                    operation_id: operation_id.clone(),
                    error_code: "recovery_abandoned".to_string(),
                    message: reason,
                },
                None,
                None,
            ));
            let has_other_pending = projection.operations.values().any(|other| {
                other.run_id == operation.run_id
                    && other.operation_id != operation_id
                    && !other.is_terminal()
            });
            if !has_other_pending {
                payloads.push((
                    EventPayload::RunCompleted {
                        run_id: operation.run_id.clone(),
                        success: false,
                    },
                    None,
                    None,
                ));
            }
        } else {
            for turn in projection
                .turns
                .values()
                .filter(|turn| turn.status == "running")
            {
                let has_pending = projection
                    .operations
                    .values()
                    .any(|operation| operation.run_id == turn.run_id && !operation.is_terminal());
                if !has_pending {
                    payloads.push((
                        EventPayload::RunCompleted {
                            run_id: turn.run_id.clone(),
                            success: false,
                        },
                        None,
                        None,
                    ));
                }
            }
        }
        let direct = self.store.direct_events(session_id)?;
        let actual_head = direct.last().map(|event| CommandHead {
            global_sequence: event.global_sequence,
            hash: event.hash.clone(),
        });
        let expected_head = expected_head.or(actual_head);
        self.store.execute_pure_command(
            command_id,
            "runtime.v1.session.recover",
            request_digest,
            Some(session_id),
            expected_head.as_ref(),
            abort_command_id,
            session_id,
            payloads,
            |events| json!({ "events": events }),
        )
    }

    pub fn export_jsonl(&self, session_id: &str) -> Result<String, SessionError> {
        let events = self.effective_events(session_id)?;
        let mut output = String::new();
        for event in events {
            output.push_str(&canonical_json(&event)?);
            output.push('\n');
        }
        Ok(output)
    }

    fn effective_events_at(
        &self,
        session_id: &str,
        max_direct_sequence: Option<i64>,
        depth: usize,
    ) -> Result<Vec<EventEnvelope>, SessionError> {
        if depth > 32 {
            return Err(SessionError::LineageTooDeep);
        }
        let direct = self.store.direct_events(session_id)?;
        let direct: Vec<_> = direct
            .into_iter()
            .filter(|event| max_direct_sequence.map_or(true, |max| event.sequence <= max))
            .collect();
        let Some(first) = direct.first() else {
            return Ok(Vec::new());
        };
        if let EventPayload::SessionForked {
            source_session_id,
            source_sequence,
            source_hash,
        } = &first.payload
        {
            let source_direct = self.store.direct_events(source_session_id)?;
            let source_anchor = source_direct
                .iter()
                .find(|event| event.sequence == *source_sequence)
                .ok_or(SessionError::ForkAnchorMismatch)?;
            if source_anchor.hash != *source_hash {
                return Err(SessionError::ForkAnchorMismatch);
            }
            let mut effective = self.effective_events_at(
                source_session_id,
                Some(*source_sequence),
                depth + 1,
            )?;
            effective.extend(direct);
            Ok(effective)
        } else {
            Ok(direct)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Proposed,
    ApprovalRequested,
    Approved,
    ExecutionRequested,
    Denied,
    Started,
    Succeeded,
    Failed,
    Unknown,
    RecoveryRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnProjection {
    pub turn_id: String,
    pub run_id: String,
    pub user_message: Option<String>,
    pub assistant_message: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OperationProjection {
    pub operation_id: String,
    pub turn_id: String,
    pub run_id: String,
    pub tool_name: String,
    pub intent: ToolIntent,
    pub request_digest: Option<String>,
    pub backend: Option<String>,
    pub external_id: Option<String>,
    pub status: OperationStatus,
    pub last_result: Option<ToolAuditResult>,
    pub last_inspection: Option<BackendOperationState>,
}

impl OperationProjection {
    pub fn is_terminal(&self) -> bool {
        matches!(
            &self.status,
            OperationStatus::Denied | OperationStatus::Succeeded | OperationStatus::Failed
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubscriptionView {
    pub subscription_id: String,
    pub session_id: String,
    pub after_global_sequence: i64,
    pub events: Vec<EventEnvelope>,
    pub deliveries: Vec<OutboxMessage>,
    pub next_global_sequence: i64,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResumeView {
    pub latest_global_sequence: i64,
    pub projection: SessionProjection,
    pub recovery_required: Vec<String>,
    pub running_turn_ids: Vec<String>,
    pub pending_model_requests: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionProjection {
    pub session_id: String,
    pub as_of_global_sequence: i64,
    pub as_of_hash: String,
    pub status: SessionStatus,
    pub workspace_roots: Vec<String>,
    pub model: Option<String>,
    pub parent: Option<(String, i64, String)>,
    pub turns: BTreeMap<String, TurnProjection>,
    pub operations: BTreeMap<String, OperationProjection>,
}

impl SessionProjection {
    pub fn empty(session_id: String) -> Self {
        Self {
            session_id,
            as_of_global_sequence: 0,
            as_of_hash: String::new(),
            status: SessionStatus::Active,
            workspace_roots: Vec::new(),
            model: None,
            parent: None,
            turns: BTreeMap::new(),
            operations: BTreeMap::new(),
        }
    }

    pub fn from_events(
        session_id: String,
        events: &[EventEnvelope],
    ) -> Result<Self, SessionError> {
        let mut projection = Self::empty(session_id);
        for event in events {
            projection.apply(event)?;
        }
        Ok(projection)
    }

    pub fn apply(&mut self, event: &EventEnvelope) -> Result<(), SessionError> {
        self.as_of_global_sequence = self.as_of_global_sequence.max(event.global_sequence);
        self.as_of_hash = event.hash.clone();
        match &event.payload {
            EventPayload::SessionCreated {
                workspace_roots,
                model,
            } => {
                self.workspace_roots = workspace_roots.clone();
                self.model = model.clone();
            }
            EventPayload::SessionForked {
                source_session_id,
                source_sequence,
                source_hash,
            } => {
                self.parent = Some((
                    source_session_id.clone(),
                    *source_sequence,
                    source_hash.clone(),
                ));
            }
            EventPayload::TurnStarted { turn_id, run_id }
            | EventPayload::RunStarted { turn_id, run_id } => {
                self.turns.entry(turn_id.clone()).or_insert(TurnProjection {
                    turn_id: turn_id.clone(),
                    run_id: run_id.clone(),
                    user_message: None,
                    assistant_message: None,
                    status: "running".to_string(),
                });
            }
            EventPayload::UserMessage { turn_id, content } => {
                if let Some(turn) = self.turns.get_mut(turn_id) {
                    turn.user_message = Some(content.clone());
                }
            }
            EventPayload::ModelResponded {
                turn_id, content, ..
            } => {
                if let Some(turn) = self.turns.get_mut(turn_id) {
                    turn.assistant_message = Some(content.clone());
                }
            }
            EventPayload::ToolProposed {
                turn_id,
                run_id,
                operation_id,
                tool_name,
                intent,
            } => {
                self.operations.insert(
                    operation_id.clone(),
                    OperationProjection {
                        operation_id: operation_id.clone(),
                        turn_id: turn_id.clone(),
                        run_id: run_id.clone(),
                        tool_name: tool_name.clone(),
                        intent: intent.clone(),
                        request_digest: None,
                        backend: None,
                        external_id: None,
                        status: OperationStatus::Proposed,
                        last_result: None,
                        last_inspection: None,
                    },
                );
            }
            EventPayload::PolicyEvaluated {
                operation_id,
                request_digest,
                outcome,
                ..
            } => {
                if let Some(operation) = self.operations.get_mut(operation_id) {
                    operation.request_digest = Some(request_digest.clone());
                    operation.status = match outcome {
                        harness_protocol::PolicyOutcome::Allowed => OperationStatus::Approved,
                        harness_protocol::PolicyOutcome::NeedsApproval => {
                            OperationStatus::ApprovalRequested
                        }
                        harness_protocol::PolicyOutcome::Denied => OperationStatus::Denied,
                    };
                }
            }
            EventPayload::ApprovalRequested { operation_id, .. } => {
                if let Some(operation) = self.operations.get_mut(operation_id) {
                    operation.status = OperationStatus::ApprovalRequested;
                }
            }
            EventPayload::ApprovalGranted { operation_id, .. } => {
                if let Some(operation) = self.operations.get_mut(operation_id) {
                    operation.status = OperationStatus::Approved;
                }
            }
            EventPayload::ApprovalDenied { operation_id, .. } => {
                if let Some(operation) = self.operations.get_mut(operation_id) {
                    operation.status = OperationStatus::Denied;
                }
            }
            EventPayload::ToolStarted {
                operation_id,
                backend,
                external_id,
            } => {
                if let Some(operation) = self.operations.get_mut(operation_id) {
                    operation.backend = Some(backend.clone());
                    operation.external_id = external_id.clone();
                    operation.status = OperationStatus::Started;
                }
            }
            EventPayload::ExecutionRequested {
                operation_id,
                backend,
                external_id,
                ..
            } => {
                if let Some(operation) = self.operations.get_mut(operation_id) {
                    operation.backend = Some(backend.clone());
                    operation.external_id = external_id.clone();
                    operation.status = OperationStatus::ExecutionRequested;
                }
            }
            EventPayload::ToolFinished {
                operation_id,
                result,
            } => {
                if let Some(operation) = self.operations.get_mut(operation_id) {
                    operation.status = match &result.status {
                        ToolStatus::Succeeded => OperationStatus::Succeeded,
                        ToolStatus::Failed | ToolStatus::TimedOut => OperationStatus::Failed,
                    };
                    operation.last_result = Some(result.clone());
                }
            }
            EventPayload::ToolFailed { operation_id, .. } => {
                if let Some(operation) = self.operations.get_mut(operation_id) {
                    operation.status = OperationStatus::Failed;
                }
            }
            EventPayload::BackendInspected {
                operation_id,
                backend,
                external_id,
                state,
                ..
            } => {
                if let Some(operation) = self.operations.get_mut(operation_id) {
                    operation.backend = Some(backend.clone());
                    if external_id.is_some() {
                        operation.external_id = external_id.clone();
                    }
                    operation.last_inspection = Some(state.clone());
                    if matches!(
                        state,
                        BackendOperationState::Running
                            | BackendOperationState::Succeeded
                            | BackendOperationState::Failed
                    ) {
                        operation.status = OperationStatus::Started;
                    } else if matches!(
                        state,
                        BackendOperationState::NotFound
                            | BackendOperationState::Unknown
                            | BackendOperationState::NotTracked
                    ) {
                        operation.status = OperationStatus::RecoveryRequired;
                    }
                }
            }
            EventPayload::ExecutionContinuationRequested {
                operation_id,
                backend,
                external_id,
            } => {
                if let Some(operation) = self.operations.get_mut(operation_id) {
                    operation.backend = Some(backend.clone());
                    operation.external_id = external_id.clone();
                    operation.status = OperationStatus::ExecutionRequested;
                    operation.last_inspection = None;
                }
            }
            EventPayload::ArtifactCreated { .. } => {}
            EventPayload::ExecutionUnknown { operation_id, .. } => {
                if let Some(operation) = self.operations.get_mut(operation_id) {
                    operation.status = OperationStatus::Unknown;
                }
            }
            EventPayload::RecoveryRequired { operation_id, .. } => {
                if let Some(operation_id) = operation_id {
                    if let Some(operation) = self.operations.get_mut(operation_id) {
                        operation.status = OperationStatus::RecoveryRequired;
                    }
                }
            }
            EventPayload::RunCompleted { run_id, success } => {
                for turn in self.turns.values_mut().filter(|turn| turn.run_id == *run_id) {
                    turn.status = if *success {
                        "completed".to_string()
                    } else {
                        "failed".to_string()
                    };
                }
            }
            EventPayload::SessionCompleted { .. } => {
                self.status = SessionStatus::Completed;
            }
            EventPayload::ModelRequested { .. }
            | EventPayload::ContextCompacted { .. }
            | EventPayload::CheckpointCreated { .. } => {}
        }
        Ok(())
    }
}

fn validate_transition(
    projection: &SessionProjection,
    payload: &EventPayload,
) -> Result<(), SessionError> {
    if matches!(&projection.status, SessionStatus::Completed)
        && !matches!(payload, EventPayload::SessionCompleted { .. })
    {
        return Err(SessionError::InvalidState(
            "completed sessions cannot receive new facts".to_string(),
        ));
    }
    match payload {
        EventPayload::SessionCreated { .. } => Err(SessionError::InvalidState(
            "session_created must be the first event".to_string(),
        )),
        EventPayload::SessionForked { .. } => Err(SessionError::InvalidState(
            "session_forked is only valid when creating a child stream".to_string(),
        )),
        EventPayload::TurnStarted { turn_id, .. } => {
            if projection.turns.contains_key(turn_id) {
                Err(SessionError::InvalidState(format!(
                    "turn `{turn_id}` already exists"
                )))
            } else {
                Ok(())
            }
        }
        EventPayload::RunStarted { turn_id, run_id } => {
            let Some(turn) = projection.turns.get(turn_id) else {
                return Err(SessionError::InvalidState(format!(
                    "turn `{turn_id}` does not exist"
                )));
            };
            if turn.run_id != *run_id {
                Err(SessionError::InvalidState(format!(
                    "run `{run_id}` is not the active run for turn `{turn_id}`"
                )))
            } else {
                Ok(())
            }
        }
        EventPayload::UserMessage { turn_id, .. }
        | EventPayload::ModelRequested { turn_id, .. }
        | EventPayload::ModelResponded { turn_id, .. }
        | EventPayload::ContextCompacted { turn_id, .. } => {
            if projection.turns.contains_key(turn_id) {
                Ok(())
            } else {
                Err(SessionError::InvalidState(format!(
                    "turn `{turn_id}` does not exist"
                )))
            }
        }
        EventPayload::ToolProposed { operation_id, .. } => {
            if projection.operations.contains_key(operation_id) {
                Err(SessionError::InvalidState(format!(
                    "operation `{operation_id}` already exists"
                )))
            } else {
                Ok(())
            }
        }
        EventPayload::PolicyEvaluated { operation_id, .. }
        | EventPayload::ApprovalRequested { operation_id, .. }
        | EventPayload::ApprovalGranted { operation_id, .. }
        | EventPayload::ApprovalDenied { operation_id, .. }
        | EventPayload::ExecutionRequested { operation_id, .. }
        | EventPayload::ToolStarted { operation_id, .. }
        | EventPayload::ToolFinished { operation_id, .. }
        | EventPayload::ToolFailed { operation_id, .. }
        | EventPayload::BackendInspected { operation_id, .. }
        | EventPayload::ExecutionContinuationRequested { operation_id, .. }
        | EventPayload::ExecutionUnknown { operation_id, .. } => {
            let Some(operation) = projection.operations.get(operation_id) else {
                return Err(SessionError::InvalidState(format!(
                    "operation `{operation_id}` does not exist"
                )));
            };
            let valid = match payload {
                EventPayload::PolicyEvaluated { .. } => matches!(
                    &operation.status,
                    OperationStatus::Proposed
                        | OperationStatus::ApprovalRequested
                        | OperationStatus::Approved
                ),
                EventPayload::ApprovalRequested { .. } => {
                    matches!(&operation.status, OperationStatus::ApprovalRequested)
                }
                EventPayload::ApprovalGranted { .. } | EventPayload::ApprovalDenied { .. } => {
                    matches!(&operation.status, OperationStatus::ApprovalRequested)
                }
                EventPayload::ExecutionRequested { .. } => {
                    matches!(&operation.status, OperationStatus::Approved)
                }
                EventPayload::ToolStarted { .. } => {
                    matches!(&operation.status, OperationStatus::ExecutionRequested)
                }
                EventPayload::ToolFinished { .. } => {
                    matches!(&operation.status, OperationStatus::Started)
                }
                EventPayload::ToolFailed { .. } => matches!(
                    &operation.status,
                    OperationStatus::Started
                        | OperationStatus::Unknown
                        | OperationStatus::RecoveryRequired
                ),
                EventPayload::ExecutionUnknown { .. } => {
                    matches!(&operation.status, OperationStatus::Started)
                }
                EventPayload::BackendInspected { .. } => matches!(
                    &operation.status,
                    OperationStatus::ExecutionRequested
                        | OperationStatus::Started
                        | OperationStatus::Unknown
                        | OperationStatus::RecoveryRequired
                ),
                EventPayload::ExecutionContinuationRequested { .. } => matches!(
                    &operation.status,
                    OperationStatus::Unknown | OperationStatus::RecoveryRequired
                ),
                _ => false,
            };
            if valid {
                Ok(())
            } else {
                Err(SessionError::InvalidState(format!(
                    "operation `{operation_id}` is not in a valid state for `{}`",
                    payload.event_type()
                )))
            }
        }
        EventPayload::ArtifactCreated { operation_id, .. } => {
            if projection.operations.contains_key(operation_id) {
                Ok(())
            } else {
                Err(SessionError::InvalidState(format!(
                    "operation `{operation_id}` does not exist"
                )))
            }
        }
        EventPayload::RecoveryRequired { operation_id, .. } => {
            if let Some(operation_id) = operation_id {
                let Some(operation) = projection.operations.get(operation_id) else {
                    return Err(SessionError::InvalidState(format!(
                        "operation `{operation_id}` does not exist"
                    )));
                };
                if operation.is_terminal() {
                    return Err(SessionError::InvalidState(format!(
                        "operation `{operation_id}` is already terminal"
                    )));
                }
            }
            Ok(())
        }
        EventPayload::RunCompleted { run_id, .. } => {
            let Some(turn) = projection.turns.values().find(|turn| turn.run_id == *run_id) else {
                return Err(SessionError::InvalidState(format!(
                    "run `{run_id}` does not exist"
                )));
            };
            if turn.status != "running" {
                return Err(SessionError::InvalidState(format!(
                    "run `{run_id}` is already terminal"
                )));
            }
            Ok(())
        }
        EventPayload::SessionCompleted { .. } => {
            if projection.status == SessionStatus::Completed {
                return Err(SessionError::InvalidState(
                    "session is already completed".to_string(),
                ));
            }
            if projection.operations.values().any(|operation| !operation.is_terminal()) {
                Err(SessionError::InvalidState(
                    "session has unresolved operations".to_string(),
                ))
            } else if projection.turns.values().any(|turn| turn.status == "running") {
                Err(SessionError::InvalidState(
                    "session has a running turn".to_string(),
                ))
            } else {
                Ok(())
            }
        }
        EventPayload::CheckpointCreated { .. } => Ok(()),
    }
}

fn configure_connection(connection: &Connection) -> Result<(), SessionError> {
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         PRAGMA foreign_keys = ON;
         PRAGMA recursive_triggers = ON;
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(())
}

fn initialize_schema(connection: &Connection) -> Result<(), SessionError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS events (
            protocol TEXT NOT NULL,
            version INTEGER NOT NULL,
            schema_version INTEGER NOT NULL,
            event_id TEXT NOT NULL UNIQUE,
            session_id TEXT NOT NULL,
            sequence INTEGER NOT NULL,
            global_sequence INTEGER NOT NULL UNIQUE,
            recorded_at_ms INTEGER NOT NULL,
            correlation_id TEXT,
            causation_id TEXT,
            prev_hash TEXT NOT NULL,
            hash TEXT NOT NULL,
            event_type TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            PRIMARY KEY (session_id, sequence)
         );
         CREATE INDEX IF NOT EXISTS events_global_sequence_idx
           ON events(global_sequence);
         CREATE TABLE IF NOT EXISTS operation_claims (
            session_id TEXT NOT NULL,
            operation_id TEXT NOT NULL,
            proposal_event_id TEXT NOT NULL UNIQUE,
            PRIMARY KEY (session_id, operation_id)
         );
         CREATE TABLE IF NOT EXISTS turn_claims (
            session_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            start_event_id TEXT NOT NULL UNIQUE,
            PRIMARY KEY (session_id, turn_id)
         );
         CREATE TABLE IF NOT EXISTS command_receipts (
            command_id TEXT PRIMARY KEY,
            method TEXT NOT NULL,
            request_digest TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('pending', 'committed', 'rejected', 'aborted')),
            result_json TEXT,
            error_json TEXT,
            event_ids_json TEXT NOT NULL DEFAULT '[]',
            created_at_ms INTEGER NOT NULL,
            completed_at_ms INTEGER
         );
         CREATE INDEX IF NOT EXISTS command_receipts_state_idx
           ON command_receipts(state, created_at_ms);
         CREATE TABLE IF NOT EXISTS subscriptions (
            subscription_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            after_global_sequence INTEGER NOT NULL DEFAULT 0,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS subscriptions_session_idx
           ON subscriptions(session_id, updated_at_ms);
         CREATE TABLE IF NOT EXISTS subscription_outbox (
             message_id INTEGER PRIMARY KEY AUTOINCREMENT,
             subscription_id TEXT NOT NULL,
             event_id TEXT NOT NULL,
             session_id TEXT NOT NULL,
             global_sequence INTEGER NOT NULL,
             event_json TEXT NOT NULL,
             state TEXT NOT NULL CHECK (state IN ('pending', 'in_flight', 'acked')),
             delivery_attempts INTEGER NOT NULL DEFAULT 0,
             available_at_ms INTEGER NOT NULL,
             lease_until_ms INTEGER,
             last_error TEXT,
             updated_at_ms INTEGER NOT NULL,
             UNIQUE (subscription_id, event_id),
             FOREIGN KEY (subscription_id) REFERENCES subscriptions(subscription_id)
          );
          CREATE INDEX IF NOT EXISTS subscription_outbox_claim_idx
            ON subscription_outbox(subscription_id, state, available_at_ms, global_sequence);
          CREATE TABLE IF NOT EXISTS artifacts (
             artifact_id TEXT PRIMARY KEY,
             session_id TEXT NOT NULL,
             operation_id TEXT NOT NULL,
             kind TEXT NOT NULL,
             sha256 TEXT NOT NULL,
             bytes INTEGER NOT NULL,
             media_type TEXT NOT NULL,
             content TEXT NOT NULL,
             created_at_ms INTEGER NOT NULL
          );
          CREATE INDEX IF NOT EXISTS artifacts_session_idx
            ON artifacts(session_id, created_at_ms);
          CREATE TRIGGER IF NOT EXISTS artifacts_no_update
            BEFORE UPDATE ON artifacts
            BEGIN SELECT RAISE(ABORT, 'artifacts are immutable'); END;
          CREATE TRIGGER IF NOT EXISTS artifacts_no_delete
            BEFORE DELETE ON artifacts
            BEGIN SELECT RAISE(ABORT, 'artifacts are immutable'); END;
          CREATE TRIGGER IF NOT EXISTS events_to_subscription_outbox
          AFTER INSERT ON events
          BEGIN
            INSERT OR IGNORE INTO subscription_outbox
              (subscription_id, event_id, session_id, global_sequence, event_json,
               state, delivery_attempts, available_at_ms, updated_at_ms)
            SELECT subscription_id, NEW.event_id, NEW.session_id, NEW.global_sequence,
                   json_object(
                     'protocol', NEW.protocol,
                     'version', NEW.version,
                     'schema_version', NEW.schema_version,
                     'event_id', NEW.event_id,
                     'session_id', NEW.session_id,
                     'sequence', NEW.sequence,
                     'global_sequence', NEW.global_sequence,
                     'recorded_at_ms', NEW.recorded_at_ms,
                     'correlation_id', NEW.correlation_id,
                     'causation_id', NEW.causation_id,
                     'prev_hash', NEW.prev_hash,
                     'hash', NEW.hash,
                     'payload', json(NEW.payload_json)
                   ), 'pending', 0, NEW.recorded_at_ms, NEW.recorded_at_ms
            FROM subscriptions
            WHERE subscriptions.session_id = NEW.session_id
              AND NEW.global_sequence > subscriptions.after_global_sequence;
          END;
          CREATE TRIGGER IF NOT EXISTS events_no_update
           BEFORE UPDATE ON events
           BEGIN SELECT RAISE(ABORT, 'event log is append-only'); END;
         CREATE TRIGGER IF NOT EXISTS events_no_delete
           BEFORE DELETE ON events
           BEGIN SELECT RAISE(ABORT, 'event log is append-only'); END;",
    )?;
    Ok(())
}

fn append_in_transaction(
    transaction: &Transaction<'_>,
    session_id: &str,
    payload: EventPayload,
    correlation_id: Option<String>,
    causation_id: Option<String>,
) -> Result<EventEnvelope, SessionError> {
    let sequence: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM events WHERE session_id = ?1",
        [session_id],
        |row| row.get(0),
    )?;
    let global_sequence: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(global_sequence), 0) + 1 FROM events",
        [],
        |row| row.get(0),
    )?;
    let prev_hash: Option<String> = transaction
        .query_row(
            "SELECT hash FROM events WHERE session_id = ?1 ORDER BY sequence DESC LIMIT 1",
            [session_id],
            |row| row.get(0),
        )
        .optional()?;
    let mut event = EventEnvelope::new(
        session_id,
        sequence,
        payload,
        correlation_id,
        causation_id,
    );
    event.global_sequence = global_sequence;
    event.prev_hash = prev_hash.unwrap_or_else(|| "sha256:genesis".to_string());
    event.hash = hash_event(&event)?;
    if let EventPayload::TurnStarted { turn_id, .. } = &event.payload {
        transaction.execute(
            "INSERT INTO turn_claims (session_id, turn_id, start_event_id)
             VALUES (?1, ?2, ?3)",
            params![&event.session_id, turn_id, &event.event_id],
        )?;
    }
    if let EventPayload::ToolProposed { operation_id, .. } = &event.payload {
        transaction.execute(
            "INSERT INTO operation_claims (session_id, operation_id, proposal_event_id)
             VALUES (?1, ?2, ?3)",
            params![&event.session_id, operation_id, &event.event_id],
        )?;
    }
    let payload_json = serde_json::to_string(&event.payload)?;
    transaction.execute(
        "INSERT INTO events (
            protocol, version, schema_version, event_id, session_id, sequence,
            global_sequence, recorded_at_ms, correlation_id, causation_id,
            prev_hash, hash, event_type, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            &event.protocol,
            event.version,
            event.schema_version,
            &event.event_id,
            &event.session_id,
            event.sequence,
            event.global_sequence,
            event.recorded_at_ms,
            &event.correlation_id,
            &event.causation_id,
            &event.prev_hash,
            &event.hash,
            event.event_type(),
            payload_json,
        ],
    )?;
    Ok(event)
}

fn decode_command_row(row: &Row<'_>) -> rusqlite::Result<CommandReceipt> {
    let state: String = row.get(3)?;
    let state = match state.as_str() {
        "pending" => CommandState::Pending,
        "committed" => CommandState::Committed,
        "rejected" => CommandState::Rejected,
        "aborted" => CommandState::Aborted,
        _ => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(io::Error::new(io::ErrorKind::InvalidData, "unknown command state")),
            ));
        }
    };
    let result_json: Option<String> = row.get(4)?;
    let error_json: Option<String> = row.get(5)?;
    let event_ids_json: String = row.get(6)?;
    let result = result_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
    let error = error_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
    let event_ids = serde_json::from_str(&event_ids_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    Ok(CommandReceipt {
        command_id: row.get(0)?,
        method: row.get(1)?,
        request_digest: row.get(2)?,
        state,
        result,
        error,
        event_ids,
        created_at_ms: row.get(7)?,
        completed_at_ms: row.get(8)?,
    })
}

fn decode_event_row(row: &Row<'_>) -> rusqlite::Result<EventEnvelope> {
    let stored_event_type: String = row.get(12)?;
    let payload_json: String = row.get(13)?;
    let raw_payload: serde_json::Value = serde_json::from_str(&payload_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            13,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    let payload: EventPayload = serde_json::from_value(raw_payload.clone()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            13,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    if stored_event_type != payload.event_type() {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            12,
            rusqlite::types::Type::Text,
            Box::new(io::Error::new(io::ErrorKind::InvalidData, "event type mismatch")),
        ));
    }
    let event = EventEnvelope {
        protocol: row.get(0)?,
        version: row.get(1)?,
        schema_version: row.get(2)?,
        event_id: row.get(3)?,
        session_id: row.get(4)?,
        sequence: row.get(5)?,
        global_sequence: row.get(6)?,
        recorded_at_ms: row.get(7)?,
        correlation_id: row.get(8)?,
        causation_id: row.get(9)?,
        prev_hash: row.get(10)?,
        hash: row.get(11)?,
        payload,
    };
    if event.protocol != PROTOCOL_NAME
        || event.version != PROTOCOL_VERSION
        || !matches!(event.schema_version, 1 | EVENT_SCHEMA_VERSION)
    {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(io::Error::new(io::ErrorKind::InvalidData, "unsupported event schema")),
        ));
    }
    let stored_hash = event.hash.clone();
    let mut hashed_value = serde_json::to_value(&event).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            11,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    if let Some(object) = hashed_value.as_object_mut() {
        object.insert("payload".to_string(), raw_payload);
        object.insert("hash".to_string(), serde_json::Value::Null);
    }
    let calculated_hash = hash_value(&hashed_value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            11,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    if calculated_hash != stored_hash {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            11,
            rusqlite::types::Type::Text,
            Box::new(io::Error::new(io::ErrorKind::InvalidData, "event hash mismatch")),
        ));
    }
    Ok(event)
}

fn hash_event(event: &EventEnvelope) -> Result<String, SessionError> {
    let mut value = serde_json::to_value(event)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("hash".to_string(), serde_json::Value::Null);
    }
    hash_value(&value)
}

fn hash_value(value: &serde_json::Value) -> Result<String, SessionError> {
    let canonical = canonical_json(value)?;
    let digest = Sha256::digest(canonical.as_bytes());
    Ok(format!("sha256:{digest:x}"))
}

fn verify_hash_chain(session_id: &str, events: &[EventEnvelope]) -> Result<(), SessionError> {
    let mut previous_hash = "sha256:genesis".to_string();
    for (index, event) in events.iter().enumerate() {
        if event.session_id != session_id
            || event.sequence != index as i64 + 1
            || event.prev_hash != previous_hash
            || hash_event(event)? != event.hash
        {
            return Err(SessionError::InvalidHash {
                session_id: session_id.to_string(),
                sequence: event.sequence,
            });
        }
        previous_hash = event.hash.clone();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::{now_ms, EventPayload, ToolIntent};

    #[test]
    fn append_replay_and_hash_chain_are_durable() {
        let store = SqliteEventStore::in_memory().unwrap();
        let engine = SessionEngine::new(store.clone());
        let (session_id, created) = engine
            .create_session(vec!["workspace".into()], Some("gpt-5.4".into()))
            .unwrap();
        assert_eq!(created.sequence, 1);
        let turn_id = "turn-1".to_string();
        let run_id = "run-1".to_string();
        engine
            .append(
                &session_id,
                EventPayload::TurnStarted {
                    turn_id: turn_id.clone(),
                    run_id: run_id.clone(),
                },
                None,
                None,
            )
            .unwrap();
        engine
            .append(
                &session_id,
                EventPayload::UserMessage {
                    turn_id,
                    content: "hello".into(),
                },
                None,
                None,
            )
            .unwrap();
        let projection = engine.replay(&session_id).unwrap();
        assert_eq!(projection.as_of_global_sequence, 3);
        assert_eq!(projection.turns["turn-1"].user_message.as_deref(), Some("hello"));
        assert!(store.rebuild_check().is_ok());
    }

    #[test]
    fn fork_references_parent_without_copying_events() {
        let engine = SessionEngine::new(SqliteEventStore::in_memory().unwrap());
        let (source, _) = engine.create_session(vec!["workspace".into()], None).unwrap();
        engine
            .append(
                &source,
                EventPayload::TurnStarted {
                    turn_id: "t".into(),
                    run_id: "r".into(),
                },
                None,
                None,
            )
            .unwrap();
        let (child, fork_event) = engine.fork(&source, 2).unwrap();
        assert_eq!(engine.store().direct_events(&child).unwrap().len(), 1);
        assert!(matches!(fork_event.payload, EventPayload::SessionForked { .. }));
        assert_eq!(engine.effective_events(&child).unwrap().len(), 3);
        assert!(engine.replay(&child).unwrap().parent.is_some());
    }

    #[test]
    fn pending_operation_blocks_fork() {
        let engine = SessionEngine::new(SqliteEventStore::in_memory().unwrap());
        let (source, _) = engine.create_session(vec!["workspace".into()], None).unwrap();
        engine
            .append(
                &source,
                EventPayload::TurnStarted {
                    turn_id: "t".into(),
                    run_id: "r".into(),
                },
                None,
                None,
            )
            .unwrap();
        engine
            .append(
                &source,
                EventPayload::ToolProposed {
                    turn_id: "t".into(),
                    run_id: "r".into(),
                    operation_id: "op".into(),
                    tool_name: "read".into(),
                    intent: ToolIntent::ReadFile { path: "a".into() },
                },
                None,
                None,
            )
            .unwrap();
        assert!(matches!(
            engine.fork(&source, 3),
            Err(SessionError::PendingOperation(_))
        ));
    }

    #[test]
    fn append_only_trigger_rejects_delete() {
        let store = SqliteEventStore::in_memory().unwrap();
        let engine = SessionEngine::new(store.clone());
        let (session_id, _) = engine.create_session(Vec::new(), None).unwrap();
        let connection = store.connection.lock().unwrap();
        let error = connection
            .execute("DELETE FROM events WHERE session_id = ?1", [session_id])
            .unwrap_err();
        assert!(error.to_string().contains("append-only"));
    }

    #[test]
    fn command_receipt_is_durable_and_replayed_without_new_events() {
        let store = SqliteEventStore::in_memory().unwrap();
        assert_eq!(
            store
                .claim_command("cmd-1", "session.create", "digest-1", None, None)
                .unwrap(),
            CommandClaim::New
        );
        let receipt = store
            .complete_command(
                "cmd-1",
                serde_json::json!({ "session_id": "s" }),
                &["event-1".to_string()],
            )
            .unwrap();
        assert_eq!(receipt.state, CommandState::Committed);
        assert_eq!(receipt.event_ids, vec!["event-1"]);
        let replay = store
            .claim_command("cmd-1", "session.create", "digest-1", None, None)
            .unwrap();
        assert_eq!(replay, CommandClaim::Existing(receipt));
        assert!(matches!(
            store.claim_command("cmd-1", "session.create", "different", None, None),
            Err(SessionError::CommandIdempotencyConflict)
        ));
    }

    #[test]
    fn atomic_fork_command_replays_without_duplicate_child_event() {
        let store = SqliteEventStore::in_memory().unwrap();
        let engine = SessionEngine::new(store.clone());
        let (source, _) = engine.create_session(Vec::new(), None).unwrap();
        engine
            .append(
                &source,
                EventPayload::TurnStarted {
                    turn_id: "turn".into(),
                    run_id: "run".into(),
                },
                None,
                None,
            )
            .unwrap();
        let result = engine
            .fork_session_command("fork-command", "digest", &source, 2, None)
            .unwrap();
        let child = match &result {
            PureCommandResult::New { receipt, .. } => receipt
                .result
                .as_ref()
                .and_then(|result| result.get("session_id"))
                .and_then(Value::as_str)
                .unwrap()
                .to_string(),
            PureCommandResult::Existing(_) => panic!("first fork cannot replay"),
        };
        let replay = engine
            .fork_session_command("fork-command", "digest", &source, 2, None)
            .unwrap();
        assert!(matches!(replay, PureCommandResult::Existing(_)));
        assert_eq!(store.direct_events(&child).unwrap().len(), 1);
    }

    #[test]
    fn expected_head_rejects_a_stale_command_claim() {
        let store = SqliteEventStore::in_memory().unwrap();
        let engine = SessionEngine::new(store.clone());
        let (session_id, created) = engine.create_session(Vec::new(), None).unwrap();
        let stale = CommandHead {
            global_sequence: created.global_sequence,
            hash: created.hash.clone(),
        };
        engine
            .append(
                &session_id,
                EventPayload::TurnStarted {
                    turn_id: "turn".into(),
                    run_id: "run".into(),
                },
                None,
                None,
            )
            .unwrap();
        assert!(matches!(
            store.claim_command(
                "cas-1",
                "runtime.v1.turn.start",
                "digest",
                Some(&session_id),
                Some(&stale),
            ),
            Err(SessionError::ExpectedHeadMismatch { .. })
        ));
    }

    #[test]
    fn subscription_outbox_leases_retry_and_ack_durably() {
        let engine = SessionEngine::new(SqliteEventStore::in_memory().unwrap());
        let (session_id, _) = engine.create_session(Vec::new(), None).unwrap();
        let subscription = engine.subscribe(&session_id, 0, 10).unwrap();
        assert_eq!(subscription.deliveries.len(), 1);
        let created_cursor = subscription.next_global_sequence;
        engine
            .acknowledge_subscription(&subscription.subscription_id, created_cursor)
            .unwrap();
        engine
            .append(
                &session_id,
                EventPayload::TurnStarted {
                    turn_id: "turn".into(),
                    run_id: "run".into(),
                },
                None,
                None,
            )
            .unwrap();
        let delivery = engine
            .claim_outbox(&subscription.subscription_id, 10, 60_000)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(delivery.delivery_attempts, 1);
        engine
            .nack_outbox(&subscription.subscription_id, delivery.message_id, "retry", 0)
            .unwrap();
        let retry = engine
            .claim_outbox(&subscription.subscription_id, 10, 60_000)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(retry.message_id, delivery.message_id);
        assert_eq!(retry.delivery_attempts, 2);
    }

    #[test]
    fn terminal_tool_facts_and_artifacts_commit_together() {
        let engine = SessionEngine::new(SqliteEventStore::in_memory().unwrap());
        let (session_id, _) = engine.create_session(Vec::new(), None).unwrap();
        engine
            .append(
                &session_id,
                EventPayload::TurnStarted {
                    turn_id: "turn".into(),
                    run_id: "run".into(),
                },
                None,
                None,
            )
            .unwrap();
        engine
            .append(
                &session_id,
                EventPayload::ToolProposed {
                    turn_id: "turn".into(),
                    run_id: "run".into(),
                    operation_id: "operation".into(),
                    tool_name: "read".into(),
                    intent: ToolIntent::ReadFile { path: "a.txt".into() },
                },
                None,
                None,
            )
            .unwrap();
        engine
            .append(
                &session_id,
                EventPayload::PolicyEvaluated {
                    operation_id: "operation".into(),
                    request_digest: "digest".into(),
                    policy_version: "policy".into(),
                    outcome: harness_protocol::PolicyOutcome::Allowed,
                    reason: "test".into(),
                },
                None,
                None,
            )
            .unwrap();
        engine
            .append(
                &session_id,
                EventPayload::ExecutionRequested {
                    operation_id: "operation".into(),
                    request_digest: "digest".into(),
                    backend: "local-trusted-host".into(),
                    external_id: None,
                },
                None,
                None,
            )
            .unwrap();
        engine
            .append(
                &session_id,
                EventPayload::ToolStarted {
                    operation_id: "operation".into(),
                    backend: "local-trusted-host".into(),
                    external_id: None,
                },
                None,
                None,
            )
            .unwrap();
        let (_, audit) = engine
            .finish_operation(
                &session_id,
                "operation",
                "turn",
                "run",
                ToolAuditResult {
                    status: ToolStatus::Succeeded,
                    exit_code: Some(0),
                    stdout_digest: "stdout".into(),
                    stderr_digest: "stderr".into(),
                    stdout_bytes: 5,
                    stderr_bytes: 0,
                    bytes_written: None,
                    output_truncated: false,
                    duration_ms: 1,
                    stdout_artifact_id: None,
                    stderr_artifact_id: None,
                },
                vec![ArtifactInput {
                    kind: "stdout".into(),
                    content: "hello".into(),
                    media_type: "text/plain".into(),
                }],
                true,
            )
            .unwrap();
        let artifact_id = audit.stdout_artifact_id.unwrap();
        assert_eq!(
            engine
                .get_artifact(&session_id, &artifact_id)
                .unwrap()
                .unwrap()
                .content,
            "hello"
        );
        let projection = engine.replay(&session_id).unwrap();
        assert_eq!(
            &projection.operations["operation"].status,
            &OperationStatus::Succeeded
        );
        assert_eq!(projection.turns["turn"].status, "completed");
    }

    #[test]
    fn client_artifacts_are_put_and_listed_for_existing_sessions() {
        let engine = SessionEngine::new(SqliteEventStore::in_memory().unwrap());
        let (session_id, _) = engine.create_session(Vec::new(), None).unwrap();

        let metadata = engine
            .put_artifact(
                &session_id,
                "client-upload",
                "input",
                "hello 🌍",
                "text/plain",
            )
            .unwrap();
        assert_eq!(metadata.operation_id, "client-upload");
        assert_eq!(metadata.bytes, "hello 🌍".as_bytes().len() as u64);
        let listed = engine.list_artifacts(&session_id).unwrap();
        assert_eq!(listed, vec![metadata]);
        let artifact_id = listed[0].artifact_id.clone();
        assert_eq!(
            engine
                .get_artifact(&session_id, &artifact_id)
                .unwrap()
                .unwrap()
                .content,
            "hello 🌍"
        );
        assert!(matches!(
            engine.put_artifact("missing", "client-upload", "input", "content", "text/plain"),
            Err(SessionError::SessionNotFound(session)) if session == "missing"
        ));
    }

    #[test]
    fn timestamp_helper_is_non_negative() {
        assert!(now_ms() >= 0);
    }
}
