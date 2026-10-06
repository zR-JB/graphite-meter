use super::*;
use crate::{auth::Holder, exchange::Exchange, lane::Work};
use graphite_meter_proto::upload::Record;
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
    let uploads = Uploads::new([7; 32], Meter::new(true));
    let id = uploads.mint();
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

#[tokio::test(start_paused = true)]
async fn ids_are_signed_by_their_store_and_create_state_only_while_fresh() {
    let (uploads, owner, id) = fixture();
    assert!(id.starts_with("gmu_") && id.len() == 79, "{id}");
    assert_ne!(uploads.mint(), id);
    assert_eq!(uploads.live(), 0, "minting holds no state");
    let mut forged = id.clone().into_bytes();
    forged[20] = if forged[20] == b'A' { b'B' } else { b'A' };
    let forged = String::from_utf8(forged).unwrap();
    for refused in [forged.as_str(), "gmu_", "", &id[..78]] {
        assert_eq!(uploads.begin(refused, Some(&owner), lane(), None).err(), Some(UploadRefusal::Invalid));
    }
    let elsewhere = Uploads::new([8; 32], Meter::default());
    assert_eq!(elsewhere.subscribe(&id, Some(&owner)).err(), Some(UploadRefusal::Invalid));
    assert_eq!(
        uploads.checkpoint(&id, Some(&owner)).err(),
        Some(UploadRefusal::Invalid),
        "reads create nothing"
    );

    let unused = uploads.mint();
    advance(Duration::from_secs(60)).await;
    drop(uploads.begin(&id, Some(&owner), lane(), None).unwrap());
    advance(Duration::from_secs(61)).await;
    assert_eq!(uploads.begin(&unused, Some(&owner), lane(), None).err(), Some(UploadRefusal::Invalid));
    assert!(
        uploads.begin(&id, Some(&owner), lane(), None).is_ok(),
        "an aggregate outlives its ID's window"
    );
}

#[tokio::test(start_paused = true)]
async fn an_owner_is_its_narrowest_key_and_an_ambiguous_peer_owns_nothing() {
    let uploads = Uploads::new([7; 32], Meter::new(true));
    let id = uploads.mint();
    drop(
        uploads
            .begin(&id, Some(&client("2001:db8:1:2::1")), lane(), None)
            .unwrap(),
    );
    assert!(
        uploads.checkpoint(&id, Some(&client("2001:db8:1:2:ff::9"))).is_ok(),
        "one /64 is one owner"
    );
    let other = Some(client("2001:db8:1:3::1"));
    let refusals = [
        uploads.begin(&id, other.as_ref(), lane(), None).err(),
        uploads.subscribe(&id, other.as_ref()).err(),
        uploads.checkpoint(&id, other.as_ref()).err(),
        uploads.finish(&id, other.as_ref()).err(),
    ];
    assert_eq!(refusals, [Some(UploadRefusal::OwnerMismatch); 4]);

    let grant = ClientKeys::Auth(Holder::Grant("g".into()), "alice".into());
    let login = ClientKeys::Auth(Holder::Login("s".into()), "alice".into());
    let delegated = uploads.mint();
    drop(uploads.subscribe(&delegated, Some(&grant)).unwrap());
    assert_eq!(uploads.checkpoint(&delegated, Some(&login)).err(), Some(UploadRefusal::OwnerMismatch));

    let fresh = uploads.mint();
    assert_eq!(uploads.begin(&fresh, None, lane(), None).err(), Some(UploadRefusal::OwnerMismatch));
    assert_eq!(uploads.subscribe(&fresh, None).err(), Some(UploadRefusal::OwnerMismatch));
    assert_eq!(uploads.checkpoint(&fresh, None).err(), Some(UploadRefusal::Invalid));
    assert_eq!(uploads.finish(&id, None).err(), Some(UploadRefusal::OwnerMismatch));
    assert_eq!(uploads.live(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_finished_upload_completes_once_its_lanes_drain_and_takes_no_new_lane() {
    let (uploads, owner, id) = fixture();
    let mut feed = uploads.subscribe(&id, Some(&owner)).unwrap();
    assert_eq!(record(&mut feed).await, Some(Record::Ready));
    let mut lanes = [lane(), lane()].map(|lane| {
        let transfer = uploads.meter().open();
        uploads.begin(&id, Some(&owner), lane, transfer).unwrap()
    });
    lanes[0].record(100);
    lanes[1].record(230);
    assert!(matches!(record(&mut feed).await, Some(Record::Progress(counters)) if counters.bytes() == 330));
    let metered = uploads.meter().line("upload", Duration::from_secs(1)).unwrap();
    assert!(metered.ends_with(" 2 conns · 0.00 MB this window"), "{metered}");
    let finished = tokio::spawn(lanes[0].finished());
    tokio::task::yield_now().await;
    assert!(!finished.is_finished(), "a lane sees no finish before it");
    uploads.finish(&id, Some(&owner)).unwrap();
    finished.await.unwrap();
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
    assert_eq!(uploads.begin(&id, Some(&owner), lane(), None).err(), Some(UploadRefusal::Invalid));
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
    let sink = uploads.begin(&id, Some(&owner), lane(), None).unwrap();
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
