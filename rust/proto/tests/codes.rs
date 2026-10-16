use graphite_meter_proto::{lane::LaneEnding, reason::FailureReason, refusal::UploadRefusal, route::Route};

/// The fields of each row of a shared code list, comments and blank lines left out.
fn rows(list: &str) -> Vec<Vec<String>> {
    list.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| line.split('|').map(|field| field.trim().to_owned()).collect())
        .collect()
}

fn table<T: Copy>(all: &[T], row: impl Fn(T) -> Vec<String>) -> Vec<Vec<String>> {
    all.iter().map(|&item| row(item)).collect()
}

#[test]
fn routes_equal_the_route_list_row_for_row() {
    let routes = table(Route::ALL, |route| {
        vec![route.name().into(), route.path().into(), route.kind().name().into()]
    });
    assert_eq!(routes, rows(include_str!("../../../api/routes.txt")));
}

#[test]
fn failure_reasons_equal_their_list() {
    let reasons = table(FailureReason::ALL, |reason| vec![reason.key().into(), reason.label().into()]);
    assert_eq!(reasons, rows(include_str!("../../../api/failurereasons.txt")));
}

#[test]
fn lane_endings_equal_their_list() {
    let endings = table(LaneEnding::ALL, |ending| {
        let codes = [ending.websocket_code().to_string(), ending.webtransport_code().to_string()];
        [vec![ending.name().into()], codes.into(), vec![ending.reason().into()]].concat()
    });
    assert_eq!(endings, rows(include_str!("../../../api/laneendings.txt")));
}

#[test]
fn an_upload_ending_idle_or_revoked_is_refused_with_the_same_name_and_text() {
    for ending in LaneEnding::ALL {
        let refusal = ending.upload_refusal();
        let expected = matches!(ending, LaneEnding::Idle | LaneEnding::Revoked);
        assert_eq!(refusal.is_some(), expected, "{ending:?}");
        if let Some(refusal) = refusal {
            assert_eq!((refusal.name(), refusal.message()), (ending.name(), ending.reason()));
        }
    }
}

#[test]
fn upload_refusals_equal_their_list() {
    let refusals = table(UploadRefusal::ALL, |refusal| {
        vec![refusal.name().into(), refusal.message().into(), refusal.status().as_u16().to_string()]
    });
    assert_eq!(refusals, rows(include_str!("../../../api/uploadrefusals.txt")));
}

#[test]
fn each_upload_refusal_maps_by_name_to_one_failure_reason() {
    let rows = rows(include_str!("../../../api/uploadrefusalreasons.txt"));
    for refusal in UploadRefusal::ALL {
        let matching: Vec<_> = rows.iter().filter(|row| row[0] == refusal.name()).collect();
        assert_eq!(matching.len(), 1, "{refusal:?}");
        assert_eq!(refusal.failure_reason().key(), matching[0][1], "{refusal:?}");
    }
    assert_eq!(rows.len(), UploadRefusal::ALL.len());
}
