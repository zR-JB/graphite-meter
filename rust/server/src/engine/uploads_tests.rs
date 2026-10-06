use super::*;
use crate::{auth::Holder, exchange::Exchange, lane::Work};
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
