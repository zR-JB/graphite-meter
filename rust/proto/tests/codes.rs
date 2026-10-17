use graphite_meter_proto::{lane::LaneEnding, reason::FailureReason, refusal::UploadRefusal, route::Route};

/// Splits each row of a shared code list into fields, leaving comments and blank lines out.
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

#[test]
fn routes_match_whole_paths_only() {
    for path in [
        "/preflight/",
        "//preflight",
        "/Preflight",
        "/%70reflight",
        "/preflight?x=1",
        "/wt",
        "/wt/",
        "",
        "/",
    ] {
        assert_eq!(Route::find_by(Route::path, path), None, "{path}");
    }
}

#[test]
fn routes_dispatch_their_methods_and_leave_head_and_options_to_the_router() {
    let methods: [(&[&str], &[&str]); 4] = [
        (&["GET"], &["/preflight", "/probe", "/download", "/servers", "/ws/ping"]),
        (
            &["POST"],
            &["/upload", "/upload/session", "/upload/checkpoint", "/wt/session", "/ws/session"],
        ),
        (&["GET", "DELETE"], &["/upload/progress"]),
        (&["CONNECT"], &["/wt/download", "/wt/upload", "/wt/ping"]),
    ];
    for (methods, paths) in methods {
        for path in paths {
            assert_eq!(Route::find_by(Route::path, *path).map(Route::methods), Some(methods), "{path}");
        }
    }
    assert_eq!(methods.iter().map(|(_, paths)| paths.len()).sum::<usize>(), Route::ALL.len());
}

// find_by answers the first match, so every column a peer's value is looked up by must be unique.
#[test]
fn every_looked_up_column_is_unique() {
    fn unique<T: Copy, V: PartialEq + std::fmt::Debug>(all: &[T], column: fn(T) -> V) {
        let values: Vec<V> = all.iter().map(|&item| column(item)).collect();
        for (index, value) in values.iter().enumerate() {
            assert!(!values[..index].contains(value), "{value:?} repeats");
        }
    }
    unique(Route::ALL, Route::path);
    unique(Route::ALL, Route::name);
    unique(LaneEnding::ALL, LaneEnding::websocket_code);
    unique(LaneEnding::ALL, LaneEnding::webtransport_code);
    unique(LaneEnding::ALL, LaneEnding::name);
    unique(UploadRefusal::ALL, UploadRefusal::name);
    unique(FailureReason::ALL, FailureReason::key);
}
