#!/usr/bin/env node

import { main } from "./web-launcher.mjs";

process.exitCode = await main("dsh");
