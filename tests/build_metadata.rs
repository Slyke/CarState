use carstate::model::BuildInfo;
use serde_json::Value;

#[test]
fn runtime_reports_embedded_build_metadata() {
    let embedded: Value =
        serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/build-info.json"))).unwrap();
    let reported = serde_json::to_value(BuildInfo::default()).unwrap();
    assert_eq!(reported, embedded);
}
