use graphite_meter_core::format;
use serde_json::Value;

#[test]
fn shared_unit_formatting() {
    let cases: Value = serde_json::from_str(include_str!("../../../api/format.testvectors.json")).unwrap();
    for (kind, cases) in cases.as_object().unwrap() {
        for case in cases.as_array().unwrap() {
            let value = case["in"].as_f64().unwrap_or_default();
            let actual = match kind.as_str() {
                "ms" => format::fixed_ms(value),
                "latency" => format::latency_ms(value),
                "added" => format::added_ms(value),
                "speed" => format::speed(value),
                "rate" => format::rate(case["bytesPerSec"].as_f64().unwrap()),
                "bytes" => format::bytes(case["in"].as_u64().unwrap()),
                _ => panic!("unknown format"),
            };
            assert_eq!(actual, case["out"].as_str().unwrap(), "{kind}: {case}");
        }
    }
}
