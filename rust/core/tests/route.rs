use graphite_meter_core::route::{ALL, lookup};

#[test]
fn routes_match_shared_pin() {
    let fixture: Vec<_> = include_str!("../../../api/routes.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split('|').map(str::trim).collect::<Vec<_>>())
        .collect();
    assert_eq!(fixture.len(), ALL.len());
    for fields in fixture {
        assert_eq!(fields.len(), 3);
        let route = lookup(fields[1]).expect("pinned route must exist");
        assert_eq!(
            [route.name(), route.path(), route.kind().as_str()],
            fields.as_slice()
        );
    }
    for (index, route) in ALL.iter().enumerate() {
        assert!(!ALL[..index].contains(route));
        assert_eq!(lookup(route.path()), Some(*route));
        assert_eq!(lookup(&format!("{}/", route.path())), None);
        assert_eq!(lookup(&format!("{}?x=1", route.path())), None);
        assert_eq!(lookup(&route.path().to_uppercase()), None);
    }
    for path in ["", "/", "/wt", "/%70robe", " /probe", "/probe/child"] {
        assert_eq!(lookup(path), None);
    }
}
