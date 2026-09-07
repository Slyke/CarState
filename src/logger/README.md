# Rust logger

A reusable Rust port of `../styleguide/logging/logger.js` and `error_gen.js`.
This folder is an independent Cargo crate; it does not initialize or modify Carstate.

## Integration

The simplest dependency setup in an application's `Cargo.toml` is:

```toml
[dependencies]
styleguide-logger = { path = "src/logger" }
```

Then import `styleguide_logger::{Logger, LogOptions, ErrorOptions}`.
To reuse it in another repository, copy this whole folder and adjust the path dependency.

For a source module instead, add the library dependencies listed in this folder's
`Cargo.toml` to the application's manifest, then declare:

```rust
#[path = "logger/mod.rs"]
mod logger;

use logger::{Logger, LogOptions, ErrorOptions};
```

This explicit path also works alongside the existing `src/logger.rs` placeholder.
Plain `mod logger;` requires that placeholder to be removed or renamed first.
The CLI's `clap` dependency is only needed when building the helper binary.
Both the crate and source-module integration paths are compiled by the tests.

## Usage

All logging methods are async and must be awaited inside a Tokio runtime.

```rust,no_run
use styleguide_logger::{Logger, LogOptions, ErrorOptions};
use serde_json::json;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let logger = Logger::from_files(
    "./src/logger/logging.example.json5",
    "./src/logger/errors.json5",
)?;

let result = logger.generate_log(LogOptions {
    level: "info".into(),
    caller: "orders::create".into(),
    logger_key: Some("ORDER_CREATED".into()),
    message: "Order created".into(),
    correlation_id: Some("request-123".into()),
    context: Some(json!({"orderId": 42})),
    ..Default::default()
}).await;

for failure in result.failures {
    // Report through your application's diagnostics or metrics if desired.
    eprintln!("{}: {}", failure.sink, failure.message);
}

let original = std::io::Error::new(std::io::ErrorKind::NotFound, "Missing order");
let wrapped = logger.wrap_error(ErrorOptions {
    caller: "orders::load".into(),
    reason: "Order lookup failed".into(),
    error_key: "ORDER_LOOKUP_FAILED".into(),
    source: Some(Box::new(original)),
    include_stack_trace: true,
    ..Default::default()
}).await;
// `wrapped` implements Display and std::error::Error, preserving the original source.
return Err(wrapped.into());
# }
```

`generate_log` returns `LogOutcome { entry, failures }`.
`generate_error` returns `ErrorOutcome { details, failures }` and emits an error event.
`wrap_error` returns a `StructuredError` with `details`, `logging_failures`, and an
owned original error accessible through `std::error::Error::source`.
Unknown keys fall back to `ERR_UNKNOWN`; if neither is mapped, `errorCode` is null
in the error details. Use a distinct key for each log/error-producing call site.

Cloning a logger shares its transports and file lock. Each call concurrently attempts
all selected sinks and waits for completion or a transport timeout. No worker queue,
global installation, or shutdown flush is required. Await the calls/tasks you start;
cancelling a logging future can interrupt delivery. File writes are serialized across
clones of one logger. Separate logger instances/processes should use separate files or
an external collector when record-level atomicity is required.

## Configuration

`logging.example.json5` contains a complete example. `LoggingConfig::from_json5`
accepts either a logging object or `{ logging: { ... } }` inside application settings.
`Logger::new(config, error_codes)` accepts already-loaded settings.
`Logger::from_config_file(path)` reads the optional `logging.errorFile` catalog;
`Logger::from_files(config_path, error_path)` uses an explicit catalog path.
Missing explicitly selected files are errors. Paths are relative to the process's
working directory, matching the JavaScript logger.

The config and error-code readers accept JSON5 comments, trailing commas, quoted or
unquoted keys, and single-quoted strings. They recursively replace whole string values
like `${API_TOKEN}` using the environment before validating the final data:

- Missing variables become JSON null.
- Defined empty variables stay empty strings.
- Other values stay exact strings, without trimming or number/boolean/JSON inference.
- Object keys and partial references such as `https://${HOST}` remain unchanged.
- Variable names follow `[A-Za-z_][A-Za-z0-9_]*`.

Typed fields still require their declared types: `${TIMEOUT}` resolves to a string and
is rejected by a numeric `timeoutMs` field. Use a JSON5 number or the retained
`LOG_HTTP_TIMEOUT_MS` compatibility variable for that field. Optional null HTTP header
values are omitted. The reusable `config::load_json5::<T>` and
`config::parse_json5_with_environment::<T>` functions apply the same rules to other
application settings without changing process environment variables.

For compatibility with the copied JavaScript logger, precedence is:

1. Built-in defaults.
2. The styleguide's direct `LOG_*`, `K8S_*`, and `ERROR_FILE_PATH` variables.
3. Explicit JSON5 settings, after reference expansion.

`LOG_CONSOLE_*`, `LOG_FILE_*`, `LOG_HTTP_*`, `LOG_SYSLOG_*`, and Kubernetes variables
documented in the styleguide are supported. `LOG_STDOUT_ENABLED/FORMAT/LEVELS`,
`LOG_STDERR_ENABLED/FORMAT/LEVELS`, and `LOG_HTTP_FORMAT` are also available.
Legacy `*_LEVELS` values use comma-separated levels; `LOG_HTTP_HEADERS` accepts a
JSON5 object. Invalid legacy values fail validation. Prefer JSON5 `${ENV_VAR}`
references for new string settings. This crate reads the process environment; it
does not load `.env` files or mutate the application's environment.

Configuration errors and transport diagnostics omit source text, header values, URL
credentials/query strings, TLS key material, and response bodies. Config types that
can contain credentials intentionally do not implement Debug or Serialize. Event
messages, context, and error source messages are caller-owned and are emitted as
supplied; do not put secrets in them.

## Sinks and gates

| Sink | Behavior |
| --- | --- |
| `console` | Enabled by default; text output. Warn/warning/error go to stderr, other levels to stdout. |
| `stdout`, `stderr` | Explicit output descriptors, disabled by default. Disable console to avoid duplicate output. |
| `file` | Creates parent directories, appends text lines or JSON Lines. Default levels: warn/error. No built-in rotation. |
| `http` | HTTP/HTTPS; configurable method, headers and timeout. Default levels: error. Non-2xx statuses are delivery failures. |
| `syslog` | UDP, TCP, or TLS; RFC5424-shaped headers and the styleguide's `[log ...]` structured data. Default levels: warn/error. |

Each sink has `enabled`, `format` (`json` or `text`), and `levels`. An empty levels array
means every level; level names are case-insensitive. Any level string is accepted for
events. A syslog unknown level uses `defaultSeverity` (info by default).

Gate selection uses the explicit `gate`, otherwise `logger_key`, otherwise the
attached `error_key`. A gate's level override happens before sink filtering.
`enabled: false` suppresses all delivery. Each sink's optional boolean controls routing;
an omitted field permits that sink. `curl` is the legacy HTTP gate alias, with `http`
taking precedence when both are present. Gates cannot enable a globally disabled sink.
Gate settings themselves never appear in serialized events.

Text templates support `{$timestamp}`, `{$level}`, `{$caller}`, `{$message}`,
`{$correlationId}`, `{$errorCode}`, `{$errorKey}`, and `{$loggerKey}`. Context, structured
errors, and enabled/nonempty Kubernetes metadata are appended as JSON. Timestamps use
UTC RFC3339 milliseconds; JSON field names match the styleguide's camelCase schema.

Syslog defaults to UDP port 514; TCP also defaults to 514, TLS to 6514. Facilities may
be names or numeric codes 0–23. UDP supports `socketType: 'udp4'` or `'udp6'`. TCP/TLS
support `framing: 'octet-counted'` (UTF-8 byte length) or `'newline'`. Newline framing
escapes embedded line breaks to keep one record per line. Network operations, including
DNS and TLS handshakes for syslog, are bounded by `timeoutMs` (2500 by default).
UDP success means the OS accepted the datagram; it is not collector acknowledgement.

HTTP and syslog TLS validate the server certificate and hostname using public roots.
Both support `tlsOptions.ca` (PEM text), `tlsOptions.caFile`, and paired PEM
`tlsOptions.cert`/`tlsOptions.key` for client authentication. Syslog accepts `servername`
for the expected certificate name when connecting to a separate address. Arbitrary
Node-specific `tlsOptions` and disabled certificate verification are not supported.
HTTP redirects are returned as failures to keep log payloads on the configured endpoint.
There are no automatic retries, which avoids duplicating events.

Rust backtraces are captured at the wrapping call site, not the original error site.
The source chain is serialized to a maximum of 32 levels to bound cyclic/custom sources.
Nested structured details include the metadata for that level; their source is stored
once in the surrounding `cause` chain to avoid duplicating it on every wrap.

## Cargo helper commands

Run from the repository root. Cargo aliases are defined in `./.cargo/config.toml`.

```sh
cargo logger-build
cargo logger-test
cargo logger-check
cargo logger-fmt -- --check
cargo logger-example

cargo error-add --error-key ORDER_CREATE_FAILED
cargo error-add --error-key ORDER_ROUTE_FAILED --prefix AB --deterministic
cargo error-edit --error-key ORDER_ROUTE_FAILED --new-error-key ORDER_HANDLER_FAILED
cargo error-edit --error-key ORDER_HANDLER_FAILED --error-code 111111111111111F
cargo error-get --error-key ORDER_HANDLER_FAILED
cargo error-search --prefix AB
cargo error-list
cargo error-validate
cargo error-validate --error-code 111111111111111F
cargo error-delete --error-key ORDER_HANDLER_FAILED
# error-rm is an alias for error-delete.
```

The aliases already include Cargo's `--` separator; pass helper flags directly as above.
Every helper accepts `--error-file ./path/errors.json5`. The default is `ERROR_FILE_PATH`,
otherwise the `errors.json5` alongside this crate at build time. `--error-input-file`
and `--error-output-file` support a separate input/output catalog. The generic form is:

```sh
cargo error-gen --help
cargo run --manifest-path ./src/logger/Cargo.toml --bin error-gen -- \
  --action add --error-file ./config/errors.json5 --error-key EXAMPLE_FAILED
```

Codes retain the JavaScript generator's 16 uppercase hexadecimal characters, SHA-256
stable portion, random or deterministic entropy, count nibble, and checksum nibble.
The library exports add/edit/delete/load/save/validate functions for use without the CLI.
A rename preserves the existing code; replacing a code requires `--error-code`.
Duplicate keys, duplicate codes, invalid checksums, and invalid mutations are rejected.
Only `add` creates a missing catalog. Read commands do not rewrite files, unless an
explicit `--error-output-file` requests an export.

Mutations atomically replace the file, preserving existing permissions and sorting keys.
They canonicalize formatting to strict JSON (valid JSON5), so comments are removed on
mutation. Environment-reference strings are preserved on edits and exports. Cooperating
CLI writers use a sibling `.lock` file to prevent lost updates. After a killed CLI
process, remove its stale lock only after confirming no other writer is running.

When copying this folder to another project, copy/adapt the Cargo aliases as needed,
or run Cargo directly with `--manifest-path`.
