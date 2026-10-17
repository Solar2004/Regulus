//! The mock is the spec of the core (docs/04-client.md §1): every profile in the mock
//! fixture must survive a round trip through the contract types unchanged.

use regulus_proto::Profile;
use serde_json::Value;

const FIXTURE: &str = include_str!("../../regulus-tauri/frontend/mock/profile.json");

/// JSON numbers are numbers to the client: `50` and `50.0` are the same value.
fn normalize(value: Value) -> Value {
    match value {
        Value::Number(number) => number.as_f64().map(Value::from).unwrap_or(Value::Null),
        Value::Array(items) => Value::Array(items.into_iter().map(normalize).collect()),
        Value::Object(map) => {
            Value::Object(map.into_iter().map(|(k, v)| (k, normalize(v))).collect())
        }
        other => other,
    }
}

#[test]
fn mock_profiles_round_trip_unchanged() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture is valid JSON");
    let profiles = fixture["profiles"]
        .as_array()
        .expect("fixture has profiles");
    assert!(!profiles.is_empty());
    for raw in profiles {
        let profile: Profile =
            serde_json::from_value(raw.clone()).expect("profile matches the contract");
        let back = serde_json::to_value(&profile).expect("profile serializes");
        assert_eq!(
            normalize(back),
            normalize(raw.clone()),
            "profile {}",
            profile.id
        );
    }
}
