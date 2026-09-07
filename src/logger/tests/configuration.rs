use serde::Deserialize;
use serde_json::{json, Value};
use styleguide_logger::{
    config::{self, Format, LoggingConfig, SyslogProtocol},
    error_codes,
};

#[test]
fn environment_references_preserve_types_and_literals() {
    let mut value = json!({
        "${NAME}": "${NAME}", "empty": "${EMPTY}", "unset": "${MISSING}",
        "nested": ["${NUMBER}", {"flag": "${FLAG}", "json": "${JSON}"}],
        "partial": "https://${NAME}", "invalid": ["${}", "${1NAME}", "${A-B}", "${NAME}\n"],
        "whitespace": "${SPACE}", "unicode": "${É}",
    });
    config::resolve_environment_references(&mut value, &|key| match key {
        "NAME" => Some("example".into()),
        "EMPTY" => Some("".into()),
        "NUMBER" => Some("42".into()),
        "FLAG" => Some("false".into()),
        "JSON" => Some("[1,2]".into()),
        "SPACE" => Some("  secret  ".into()),
        _ => None,
    });
    assert_eq!(value["${NAME}"], "example");
    assert_eq!(value["empty"], "");
    assert_eq!(value["unset"], Value::Null);
    assert_eq!(
        value["nested"],
        json!(["42", {"flag":"false", "json":"[1,2]"}])
    );
    assert_eq!(value["partial"], "https://${NAME}");
    assert_eq!(
        value["invalid"],
        json!(["${}", "${1NAME}", "${A-B}", "${NAME}\n"])
    );
    assert_eq!(value["whitespace"], "  secret  ");
    assert_eq!(value["unicode"], "${É}");
}

#[test]
fn reusable_reader_validates_after_expansion_without_leaking_values() {
    #[derive(Deserialize)]
    struct Settings {
        count: u16,
    }
    let settings: Settings =
        config::parse_json5_with_environment("{ count: 12, }", &|_| None).unwrap();
    assert_eq!(settings.count, 12);
    let result = config::parse_json5_with_environment::<Settings>("{count: '${COUNT}'}", &|_| {
        Some("secret-123".into())
    });
    let error = result.err().unwrap().to_string();
    assert!(!error.contains("secret-123"));
    assert!(
        config::parse_json5_with_environment::<Settings>("{count: '${COUNT}'}", &|_| None).is_err()
    );
}

#[test]
fn defaults_and_legacy_precedence_match_the_styleguide() {
    let default = LoggingConfig::from_json5_with_environment("{}", &|_| None).unwrap();
    assert!(default.sinks.console.enabled);
    assert_eq!(default.sinks.console.format, Format::Text);
    assert!(!default.sinks.stdout.enabled);
    assert_eq!(default.sinks.file.levels, ["warn", "error"]);
    let settings = LoggingConfig::from_json5_with_environment(
        "{http: {port: 3000}, logging: {sinks: {console: {enabled: true}, http: {headers: {'x-precedence': 'config'}}}}}",
        &|key| match key {
            "LOG_CONSOLE_ENABLED" => Some("false".into()),
            "LOG_FILE_LEVELS" => Some(" DEBUG, WARN ".into()),
            "LOG_HTTP_HEADERS" => Some("{'x-precedence':'env', 'x-only-env': 123}".into()), _ => None,
        },
    ).unwrap();
    assert!(settings.sinks.console.enabled);
    assert_eq!(settings.sinks.file.levels, ["debug", "warn"]);
    assert_eq!(settings.sinks.http.headers["x-precedence"], "config");
    assert_eq!(settings.sinks.http.headers["x-only-env"], "123");
}

#[test]
fn schema_rejects_mistakes_and_invalid_transport_settings() {
    for text in [
        "{sinks:{file:{enabeld:true}}}",
        "{sinks:{file:{enabled:true,path:'${MISSING}'}}}",
        "{sinks:{http:{enabled:true,url:'ftp://example.com'}}}",
        "{sinks:{http:{enabled:true,url:'https://example.com',timeoutMs:0}}}",
        "{sinks:{syslog:{enabled:true,protocol:'bad'}}}",
        "{sinks:{syslog:{enabled:true,facility:24}}}",
        "{sinks:{syslog:{enabled:true,port:0}}}",
    ] {
        assert!(
            LoggingConfig::from_json5_with_environment(text, &|_| None).is_err(),
            "{text}"
        );
    }
    let error = LoggingConfig::from_json5_with_environment(
        "{sinks:{http:{url:'${URL}',enabled:true}}}",
        &|_| Some("secret-value".into()),
    )
    .err()
    .unwrap();
    assert!(!error.to_string().contains("secret-value"));
}

#[test]
fn tls_defaults_and_header_references() {
    let settings = LoggingConfig::from_json5_with_environment(
        "{sinks:{syslog:{protocol:'tls'},http:{headers:{Authorization:'${AUTH}',Missing:'${ABSENT}',Empty:'${EMPTY}'}}}}",
        &|key| match key { "AUTH" => Some("Bearer test".into()), "EMPTY" => Some("".into()), _ => None },
    ).unwrap();
    assert_eq!(settings.sinks.syslog.protocol, SyslogProtocol::Tls);
    assert_eq!(settings.sinks.syslog.port(), 6514);
    assert_eq!(settings.sinks.http.headers["Authorization"], "Bearer test");
    assert_eq!(settings.sinks.http.headers["Empty"], "");
    assert!(!settings.sinks.http.headers.contains_key("Missing"));
}

#[test]
fn catalogs_support_json5_and_environment_but_reject_duplicate_keys_and_codes() {
    let codes = error_codes::parse_error_codes_with_environment(
        "{// comment\n ERR_UNKNOWN: '${CODE}',}",
        &|_| Some("0000000000000000".into()),
    )
    .unwrap();
    assert_eq!(codes["ERR_UNKNOWN"], "0000000000000000");
    for text in [
        "{A:'0000000000000000',A:'111111111111111F'}",
        "{A:'0000000000000000',B:'0000000000000000'}",
        "{A:'${MISSING}'}",
        "{A:'1111111111111110'}",
        "{A:null}",
        "{A:123}",
        "[]",
    ] {
        assert!(
            error_codes::parse_error_codes_with_environment(text, &|_| None).is_err(),
            "{text}"
        );
    }
}
