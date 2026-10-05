use super::*;
use crate::{
    auth::Holder,
    engine::feed::{HEARTBEAT_AFTER, PROGRESS_INTERVAL},
    lane::{Exchange, Work},
};
use graphite_meter_proto::{lane::IDLE_BOUND, lane::LaneEnding, upload::Record};
use tokio::time::advance;
use tokio_util::sync::CancellationToken;

fn client(address: &str) -> ClientKeys {
    ClientKeys::address(address.parse().unwrap())
}

fn lane() -> Lane {
    let hold = Quota::new(1, 1).acquire(&ClientKeys::Exempt, 1).unwrap();
    let lifetime = Duration::from_secs(24 * 3600);
    Exchange::start().admit(ClientKeys::Exempt, hold, lifetime, &Work::default(), &CancellationToken::new(), None)
}

fn fixture() -> (Uploads, ClientKeys, String) {
    let uploads = Uploads::new([7; 32]);
    let id = uploads.mint().unwrap();
    (uploads, client("192.0.2.1"), id)
}

/// The feed's next line, `Some(None)` for a heartbeat; every record must decode as the contract reads it.
async fn line(feed: &mut ProgressFeed) -> Option<Option<Record>> {
    let line = feed.next().await?;
    let text = std::str::from_utf8(&line).unwrap();
    let record = text.strip_suffix('\n').expect("one line");
    Some((!record.is_empty()).then(|| Record::decode(record.as_bytes()).unwrap()))
}

/// The feed's next record past heartbeats.
async fn record(feed: &mut ProgressFeed) -> Option<Record> {
    loop {
        if let Some(record) = line(feed).await? {
            return Some(record);
        }
    }
}

fn invalid() -> Record {
    UploadRefusal::Invalid.into()
}

#[tokio::test(start_paused = true)]
async fn ids_are_signed_by_their_store_and_create_state_only_while_fresh() {
    let (uploads, owner, id) = fixture();
    assert!(id.starts_with("gmu_") && id.len() == 79, "{id}");
    assert_ne!(uploads.mint().unwrap(), id);
    assert_eq!(uploads.live(), 0, "minting holds no state");
    let mut forged = id.clone().into_bytes();
    forged[20] = if forged[20] == b'A' { b'B' } else { b'A' };
    let forged = String::from_utf8(forged).unwrap();
    for refused in [forged.as_str(), "gmu_", "", &id[..78]] {
        assert_eq!(uploads.begin(refused, Some(&owner), lane()).err(), Some(UploadRefusal::Invalid));
    }
    let elsewhere = Uploads::new([8; 32]);
    assert_eq!(elsewhere.subscribe(&id, Some(&owner)).err(), Some(UploadRefusal::Invalid));
    assert_eq!(
        uploads.checkpoint(&id, Some(&owner)).err(),
        Some(UploadRefusal::Invalid),
        "reads create nothing"
    );

    let unused = uploads.mint().unwrap();
    advance(Duration::from_secs(60)).await;
    drop(uploads.begin(&id, Some(&owner), lane()).unwrap());
    advance(Duration::from_secs(61)).await;
    assert_eq!(uploads.begin(&unused, Some(&owner), lane()).err(), Some(UploadRefusal::Invalid));
    assert!(uploads.begin(&id, Some(&owner), lane()).is_ok(), "an aggregate outlives its ID's window");
}

#[tokio::test(start_paused = true)]
async fn an_owner_is_its_narrowest_key_and_an_ambiguous_peer_owns_nothing() {
    let uploads = Uploads::new([7; 32]);
    let id = uploads.mint().unwrap();
    drop(uploads.begin(&id, Some(&client("2001:db8:1:2::1")), lane()).unwrap());
    assert!(
        uploads.checkpoint(&id, Some(&client("2001:db8:1:2:ff::9"))).is_ok(),
        "one /64 is one owner"
    );
    let other = Some(client("2001:db8:1:3::1"));
    let refusals = [
        uploads.begin(&id, other.as_ref(), lane()).err(),
        uploads.subscribe(&id, other.as_ref()).err(),
        uploads.checkpoint(&id, other.as_ref()).err(),
        uploads.finish(&id, other.as_ref()).err(),
    ];
    assert_eq!(refusals, [Some(UploadRefusal::OwnerMismatch); 4]);

    let grant = ClientKeys::Auth(Holder::Grant("g".into()), "alice".into());
    let login = ClientKeys::Auth(Holder::Login("s".into()), "alice".into());
    let delegated = uploads.mint().unwrap();
    drop(uploads.subscribe(&delegated, Some(&grant)).unwrap());
    assert_eq!(uploads.checkpoint(&delegated, Some(&login)).err(), Some(UploadRefusal::OwnerMismatch));

    let fresh = uploads.mint().unwrap();
    assert_eq!(uploads.begin(&fresh, None, lane()).err(), Some(UploadRefusal::OwnerMismatch));
    assert_eq!(uploads.subscribe(&fresh, None).err(), Some(UploadRefusal::OwnerMismatch));
    assert_eq!(uploads.checkpoint(&fresh, None).err(), Some(UploadRefusal::Invalid));
    assert_eq!(uploads.finish(&id, None).err(), Some(UploadRefusal::OwnerMismatch));
    assert_eq!(uploads.live(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_client_holds_thirty_two_aggregates_and_its_wider_prefixes_twice_as_many() {
    let uploads = Uploads::new([7; 32]);
    let create = |keys: &ClientKeys| uploads.subscribe(&uploads.mint().unwrap(), Some(keys)).err();
    let v4 = client("192.0.2.1");
    for _ in 0..MAX_PER_CLIENT {
        assert_eq!(create(&v4), None);
    }
    assert_eq!(create(&v4), Some(UploadRefusal::ClientFull));
    let (first, second, third) = (client("2001:db8:0:1::1"), client("2001:db8:0:2::1"), client("2001:db8:0:3::1"));
    for _ in 0..MAX_PER_CLIENT {
        assert_eq!((create(&first), create(&second)), (None, None));
    }
    assert_eq!(create(&third), Some(UploadRefusal::ClientFull), "their /56 holds 64");
    assert_eq!(uploads.live(), 3 * MAX_PER_CLIENT);
}

#[tokio::test(start_paused = true)]
async fn at_capacity_only_the_stalest_empty_aggregate_is_displaced_and_stays_refused() {
    let uploads = Uploads::new([7; 32]);
    let owner = client("192.0.2.1");
    let (stalest, newer) = (uploads.mint().unwrap(), uploads.mint().unwrap());
    let mut displaced = uploads.subscribe(&stalest, Some(&owner)).unwrap();
    assert_eq!(record(&mut displaced).await, Some(Record::Ready));
    advance(Duration::from_secs(1)).await;
    drop(uploads.subscribe(&newer, Some(&owner)).unwrap());
    let finished = uploads.mint().unwrap();
    drop(uploads.subscribe(&finished, Some(&owner)).unwrap());
    uploads.finish(&finished, Some(&owner)).unwrap();
    let _laned = uploads.begin(&uploads.mint().unwrap(), Some(&owner), lane()).unwrap();
    for index in 0..MAX_LIVE - 4 {
        let keys = client(&format!("198.51.100.{}", index / 30));
        uploads
            .begin(&uploads.mint().unwrap(), Some(&keys), lane())
            .unwrap()
            .record(1);
    }
    assert_eq!(uploads.live(), MAX_LIVE);

    let newcomer = client("203.0.113.1");
    let _joined = uploads
        .begin(&uploads.mint().unwrap(), Some(&newcomer), lane())
        .unwrap();
    assert_eq!(record(&mut displaced).await, Some(invalid()));
    assert_eq!(line(&mut displaced).await, None);
    let refused = uploads.subscribe(&stalest, Some(&owner)).err();
    assert_eq!(refused, Some(UploadRefusal::Invalid), "a displaced ID is refused while it is fresh");
    let _second = uploads
        .begin(&uploads.mint().unwrap(), Some(&newcomer), lane())
        .unwrap();
    assert_eq!(uploads.checkpoint(&newer, Some(&owner)).err(), Some(UploadRefusal::Invalid));
    let full = uploads.subscribe(&uploads.mint().unwrap(), Some(&newcomer)).err();
    assert_eq!(full, Some(UploadRefusal::GlobalFull), "finished, laned and nonempty aggregates stay");
}

#[tokio::test(start_paused = true)]
async fn retention_outlasts_observers_but_not_an_idle_aggregate() {
    let (uploads, owner, id) = fixture();
    let mut sink = uploads.begin(&id, Some(&owner), lane()).unwrap();
    sink.record(42);
    advance(RETENTION * 2).await;
    assert_eq!(uploads.checkpoint(&id, Some(&owner)).unwrap().bytes(), 42, "a live lane keeps it");
    drop(sink);
    let mut feed = uploads.subscribe(&id, Some(&owner)).unwrap();
    assert_eq!(record(&mut feed).await, Some(Record::Ready));
    advance(RETENTION - Duration::from_secs(1)).await;
    assert!(uploads.checkpoint(&id, Some(&owner)).is_ok());
    drop(uploads.subscribe(&id, Some(&owner)).unwrap());
    advance(Duration::from_secs(1) + SWEEP_INTERVAL).await;
    assert_eq!(uploads.checkpoint(&id, Some(&owner)).err(), Some(UploadRefusal::Invalid));
    let mut feed = uploads.subscribe(&uploads.mint().unwrap(), Some(&owner)).unwrap();
    assert_eq!(record(&mut feed).await, Some(Record::Ready));
    advance(RETENTION + SWEEP_INTERVAL * 2).await;
    assert_eq!(record(&mut feed).await, Some(invalid()), "an expired aggregate ends its feed");
    assert_eq!(uploads.live(), 0);
}

#[tokio::test(start_paused = true)]
async fn progress_follows_every_hundred_milliseconds_after_the_first_chunk() {
    let (uploads, owner, id) = fixture();
    let mut feed = uploads.subscribe(&id, Some(&owner)).unwrap();
    let start = Instant::now();
    assert_eq!(line(&mut feed).await, Some(Some(Record::Ready)));
    assert_eq!(line(&mut feed).await, Some(None));
    assert_eq!(start.elapsed(), HEARTBEAT_AFTER, "a heartbeat after a second without a line");
    let mut sink = uploads.begin(&id, Some(&owner), lane()).unwrap();
    sink.record(10);
    let Some(Record::Progress(first)) = record(&mut feed).await else {
        panic!("no progress")
    };
    let mut last = (Instant::now(), first);
    for _ in 0..5 {
        let Some(Some(Record::Progress(counters))) = line(&mut feed).await else {
            panic!("no progress")
        };
        assert_eq!(Instant::now() - last.0, PROGRESS_INTERVAL);
        assert_eq!(counters.bytes(), 10, "unchanged bytes are reported too");
        assert_eq!(Duration::from_nanos(counters.nanos() - last.1.nanos()), PROGRESS_INTERVAL);
        last = (Instant::now(), counters);
    }
}

#[tokio::test(start_paused = true)]
async fn a_finished_upload_completes_once_its_lanes_drain_and_takes_no_new_lane() {
    let (uploads, owner, id) = fixture();
    let mut feed = uploads.subscribe(&id, Some(&owner)).unwrap();
    assert_eq!(record(&mut feed).await, Some(Record::Ready));
    let mut lanes = [lane(), lane()].map(|lane| uploads.begin(&id, Some(&owner), lane).unwrap());
    lanes[0].record(100);
    lanes[1].record(230);
    assert!(matches!(record(&mut feed).await, Some(Record::Progress(counters)) if counters.bytes() == 330));
    uploads.finish(&id, Some(&owner)).unwrap();
    let [first_sink, second_sink] = lanes;
    drop(first_sink);
    for _ in 0..3 {
        assert_eq!(line(&mut feed).await, Some(None), "only heartbeats while a lane drains");
    }
    drop(second_sink);
    let Some(Record::Complete(complete)) = record(&mut feed).await else {
        panic!("no completion")
    };
    assert_eq!(complete.bytes(), 330);
    assert!(complete.nanos() > 0);
    assert_eq!(line(&mut feed).await, None);
    assert_eq!(uploads.begin(&id, Some(&owner), lane()).err(), Some(UploadRefusal::Invalid));
    assert_eq!(uploads.checkpoint(&id, Some(&owner)).unwrap().bytes(), 330);
    let mut replay = uploads.subscribe(&id, Some(&owner)).unwrap();
    assert_eq!(record(&mut replay).await, Some(Record::Ready));
    assert!(matches!(record(&mut replay).await, Some(Record::Complete(counters)) if counters.bytes() == 330));
}

#[tokio::test(start_paused = true)]
async fn lifecycle_changes_wake_a_waiting_feed_at_once() {
    let (uploads, owner, id) = fixture();
    let waiting = |mut feed: ProgressFeed| {
        tokio::spawn(async move {
            assert_eq!(record(&mut feed).await, Some(Record::Ready));
            let line = line(&mut feed).await;
            (line, Instant::now())
        })
    };
    let replaced = waiting(uploads.subscribe(&id, Some(&owner)).unwrap());
    tokio::task::yield_now().await;
    let sink = uploads.begin(&id, Some(&owner), lane()).unwrap();
    let current = waiting(uploads.subscribe(&id, Some(&owner)).unwrap());
    let start = Instant::now();
    assert_eq!(replaced.await.unwrap(), (None, start), "a newer reader ends the older feed");
    tokio::task::yield_now().await;
    uploads.finish(&id, Some(&owner)).unwrap();
    tokio::task::yield_now().await;
    drop(sink);
    let (line, at) = current.await.unwrap();
    assert_eq!((line, at), (Some(Some(Record::Complete(Counters::new(0, 0)))), start));
}

#[tokio::test(start_paused = true)]
async fn lanes_feed_one_aggregate_and_each_chunk_moves_its_lane() {
    let (uploads, owner, id) = fixture();
    let bounded = lane();
    let mut first = uploads.begin(&id, Some(&owner), bounded.clone()).unwrap();
    let mut second = uploads.begin(&id, Some(&owner), lane()).unwrap();
    advance(Duration::from_secs(20)).await;
    first.record(5);
    first.record(0);
    second.record(7);
    assert_eq!((first.bytes(), second.bytes()), (5, 7));
    let counters = uploads.checkpoint(&id, Some(&owner)).unwrap();
    assert_eq!((counters.bytes(), counters.nanos()), (12, 0));
    let start = Instant::now();
    assert_eq!(bounded.ended().await, LaneEnding::Idle);
    assert_eq!(start.elapsed(), IDLE_BOUND, "idle from the last chunk");
    assert_eq!(uploads.checkpoint(&id, Some(&owner)).unwrap().nanos(), IDLE_BOUND.as_nanos() as u64);
}
