use super::*;
use crate::{
    auth::{Holder, Principal},
    exchange::Exchange,
    lane::Work,
};
use tokio::time::advance;
use tokio_util::sync::CancellationToken;

fn live(uploads: &Uploads) -> usize {
    uploads.entries().live.len()
}

fn client(address: &str) -> ClientKeys {
    ClientKeys::address(address.parse().unwrap())
}

fn lane() -> Lane {
    let hold = Quota::new(1, 1).acquire(&ClientKeys::Exempt, 1).unwrap();
    let lifetime = Duration::from_secs(24 * 3600);
    Exchange::start().admit(
        ClientKeys::Exempt,
        hold,
        lifetime,
        &Work::default(),
        &CancellationToken::new(),
        None,
    )
}

fn fixture() -> (Uploads, ClientKeys, String) {
    let uploads = Uploads::new([7; 32], Meter::new(true));
    let id = uploads.mint();
    (uploads, client("192.0.2.1"), id)
}

#[tokio::test(start_paused = true)]
async fn ids_are_signed_by_their_store_and_create_state_only_while_fresh() {
    let (uploads, owner, id) = fixture();
    assert!(id.starts_with("gmu_") && id.len() == 79, "{id}");
    assert_ne!(uploads.mint(), id);
    assert_eq!(live(&uploads), 0, "minting holds no state");
    let mut forged = id.clone().into_bytes();
    forged[20] = if forged[20] == b'A' { b'B' } else { b'A' };
    let forged = String::from_utf8(forged).unwrap();
    for refused in [forged.as_str(), "gmu_", "", &id[..78]] {
        assert_eq!(
            uploads.begin(refused, Some(&owner), lane(), None).err(),
            Some(UploadRefusal::Invalid)
        );
    }
    let elsewhere = Uploads::new([8; 32], Meter::default());
    assert_eq!(
        elsewhere.subscribe(&id, Some(&owner)).err(),
        Some(UploadRefusal::Invalid)
    );
    assert_eq!(
        uploads.checkpoint(&id, Some(&owner)).err(),
        Some(UploadRefusal::Invalid),
        "reads create nothing"
    );

    let unused = uploads.mint();
    advance(Duration::from_secs(60)).await;
    drop(uploads.begin(&id, Some(&owner), lane(), None).unwrap());
    advance(Duration::from_secs(61)).await;
    assert_eq!(
        uploads.begin(&unused, Some(&owner), lane(), None).err(),
        Some(UploadRefusal::Invalid)
    );
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
        uploads
            .checkpoint(&id, Some(&client("2001:db8:1:2:ff::9")))
            .is_ok(),
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

    let grant = ClientKeys::Auth(Holder::Grant("g".into()), Principal::oidc("alice"));
    let login = ClientKeys::Auth(Holder::Login("s".into()), Principal::oidc("alice"));
    let delegated = uploads.mint();
    drop(uploads.subscribe(&delegated, Some(&grant)).unwrap());
    assert_eq!(
        uploads.checkpoint(&delegated, Some(&login)).err(),
        Some(UploadRefusal::OwnerMismatch)
    );

    let fresh = uploads.mint();
    assert_eq!(
        uploads.begin(&fresh, None, lane(), None).err(),
        Some(UploadRefusal::OwnerMismatch)
    );
    assert_eq!(
        uploads.subscribe(&fresh, None).err(),
        Some(UploadRefusal::OwnerMismatch)
    );
    assert_eq!(
        uploads.checkpoint(&fresh, None).err(),
        Some(UploadRefusal::Invalid)
    );
    assert_eq!(
        uploads.finish(&id, None).err(),
        Some(UploadRefusal::OwnerMismatch)
    );
    assert_eq!(live(&uploads), 2);
}

#[test]
fn observations_from_concurrent_readers_never_regress() {
    let (uploads, owner, id) = fixture();
    let mut sink = uploads.begin(&id, Some(&owner), lane(), None).unwrap();
    sink.record(1);
    let observe = || {
        let (uploads, owner, id) = (uploads.clone(), owner.clone(), id.clone());
        std::thread::spawn(move || {
            (0..100_000)
                .map(|_| uploads.checkpoint(&id, Some(&owner)).unwrap())
                .collect::<Vec<_>>()
        })
    };
    let readers = [observe(), observe()];
    while !readers.iter().all(std::thread::JoinHandle::is_finished) {
        sink.record(1);
    }
    let mut observed: Vec<_> = readers
        .into_iter()
        .flat_map(|reader| reader.join().unwrap())
        .map(|counters| (counters.nanos(), counters.bytes()))
        .collect();
    observed.sort_unstable();
    assert!(
        observed.is_sorted_by_key(|&(_, bytes)| bytes),
        "a later observation has fewer bytes"
    );
}

async fn progress_at(feed: &mut ProgressFeed, start: Instant) -> Duration {
    let line = feed.next().await.unwrap();
    assert!(line.starts_with(b"{\"type\":\"progress\""), "{line:?}");
    start.elapsed()
}

#[tokio::test(start_paused = true)]
async fn progress_falls_on_a_grid_from_the_first_chunk_and_a_stall_skips_missed_ticks() {
    let (uploads, owner, id) = fixture();
    let mut feed = uploads.subscribe(&id, Some(&owner)).unwrap();
    assert!(
        feed.next()
            .await
            .unwrap()
            .starts_with(b"{\"type\":\"ready\"")
    );
    advance(Duration::from_millis(30)).await;
    let mut sink = uploads.begin(&id, Some(&owner), lane(), None).unwrap();
    sink.record(1);
    let start = Instant::now();
    let ms = Duration::from_millis;
    assert_eq!(progress_at(&mut feed, start).await, ms(100));
    assert_eq!(progress_at(&mut feed, start).await, ms(200));
    advance(ms(350)).await;
    let late = progress_at(&mut feed, start).await;
    assert_eq!(late, ms(550), "one record after a stall");
    assert_eq!(
        progress_at(&mut feed, start).await,
        ms(600),
        "then the grid"
    );
}
