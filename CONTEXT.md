# fnfyu harness

fnfyu harness is a local-first coding-agent workspace. It owns durable sessions and safe tool execution while keeping model providers replaceable and browser clients untrusted.

## Language

### Workspace and sessions

**Workspace**:
A user-authorized project root that bounds file and process tools.
_Avoid_: sandbox, arbitrary directory

**Session**:
A durable conversation branch whose turns, model requests, tool proposals, approvals, results, and recovery facts are event-sourced.
_Avoid_: chat, thread

**Turn**:
One user request and the agent run that answers it. A turn may propose several tool operations.
_Avoid_: message, request

**Artifact**:
Durable content produced by a tool operation and retrievable by the owning session. Event payloads store metadata and digests, not raw secrets.
_Avoid_: attachment, log file

### Models and providers

**Provider**:
A configured model endpoint and protocol adapter, such as OpenAI-compatible HTTP or Anthropic Messages.
_Avoid_: vendor, model

**Model**:
A selectable model identity owned by a provider profile. The model id is sent to the provider; it is not itself a credential.
_Avoid_: engine, agent

**Provider profile**:
The user-visible configuration for a provider, including protocol kind, endpoint, model list, and a reference to the environment variable that supplies its API key.
_Avoid_: account, secret

**Secret reference**:
The name of an environment or deployment secret that supplies a credential at runtime. fnfyu harness never persists or returns the credential value.
_Avoid_: API key, password

### Tools and execution

**Tool**:
A named capability with a schema that an agent can propose, such as `read_file`, `list_files`, `search`, or `read_image`.
_Avoid_: arbitrary function, command

**Tool intent**:
The typed, policy-checkable request created from a tool call. It contains no implicit shell string or hidden side effect.
_Avoid_: action, instruction

**Execution backend**:
The adapter that runs an authorized tool intent, such as the trusted host or an isolated container.
_Avoid_: runner, shell

**Workbench**:
The browser client where users manage sessions, models, providers, approvals, artifacts, and event history.
_Avoid_: dashboard, console
