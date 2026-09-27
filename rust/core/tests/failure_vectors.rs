use graphite_meter_core::failure::{FailureReason, LaneEnding, UploadRefusal};

fn rows(source: &str) -> impl Iterator<Item = Vec<&str>> {
    source
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| line.split('|').map(str::trim).collect())
}

#[test]
fn protocol_endings_and_refusals_match_shared_pins() {
    for row in rows(include_str!("../../../api/laneendings.txt")) {
        let ws = row[1].parse().unwrap();
        let wt = row[2].parse().unwrap();
        let ending = LaneEnding::from_websocket_code(ws).unwrap();
        assert_eq!(LaneEnding::from_webtransport_code(wt), Some(ending));
        assert_eq!(ending.name(), row[0]);
        assert_eq!(ending.websocket_code(), ws);
        assert_eq!(ending.webtransport_code(), wt);
        assert_eq!(ending.reason(), row[3]);
    }
    for row in rows(include_str!("../../../api/uploadrefusals.txt")) {
        let refusal = UploadRefusal::from_name(row[0]).unwrap();
        assert_eq!(refusal.name(), row[0]);
        assert_eq!(refusal.message(), row[1]);
        assert_eq!(refusal.status(), row[2].parse::<u16>().unwrap());
    }
    for (reason, row) in [
        FailureReason::PreparationFailed,
        FailureReason::ConnectionLost,
        FailureReason::Timeout,
        FailureReason::SignInRequired,
        FailureReason::ServerBusy,
        FailureReason::ProtocolError,
        FailureReason::InsufficientEvidence,
    ]
    .into_iter()
    .zip(rows(include_str!("../../../api/failurereasons.txt")))
    {
        assert_eq!(reason.name(), row[0]);
        assert_eq!(reason.label(), row[1]);
    }
}
