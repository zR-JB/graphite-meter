//! Sign-in approval: its token exchange against a canned server, and its polling on paused time.
use graphite_meter_client::net::{
    Client, Fault,
    approval::{Approval, Unapproved},
};
use graphite_meter_net::Pool;
use graphite_meter_proto::{origin::Origin, reason::FailureReason};
use std::{assert_matches, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::{Instant, sleep},
};

const SECOND: Duration = Duration::from_secs(1);

fn fresh() -> Approval {
    Approval::new(&Origin::parse("https://meter.example").unwrap())
}

#[tokio::test]
async fn the_exchange_posts_the_verifier_and_reads_the_grant() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = Origin::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let approval = Approval::new(&origin);
    let page = approval.url.clone();
    let served = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"}") {
            request.push(stream.read_u8().await.unwrap());
        }
        let answer = "HTTP/1.1 200 OK\r\ncontent-length: 19\r\n\r\n{\"token\":\"granted\"}";
        stream.write_all(answer.as_bytes()).await.unwrap();
        String::from_utf8(request).unwrap()
    });
    let granted = approval.grant(&Client::new(false, Arc::new(Pool::inline()))).await;
    assert_eq!(granted, Ok("granted".into()));
    let request = served.await.unwrap();
    assert!(request.starts_with("POST /auth/cli/token HTTP/1.1\r\n"), "{request}");
    let verifier = request.split_once("\r\n\r\n").unwrap().1;
    assert!(verifier.starts_with(r#"{"verifier":""#) && verifier.len() == 79, "{verifier}");
    assert!(page.starts_with(&format!("{origin}/auth/cli?challenge=")), "{page}");
}

#[tokio::test(start_paused = true)]
async fn polls_go_every_second_and_a_slow_one_is_followed_at_once() {
    let (approval, started, mut polls) = (fresh(), Instant::now(), Vec::new());
    let granted = approval
        .wait(|| {
            polls.push(started.elapsed());
            let (slow, granted) = (polls.len() == 2, polls.len() == 4);
            async move {
                if slow {
                    sleep(SECOND * 3 / 2).await;
                }
                Ok(granted.then(|| "granted".into()))
            }
        })
        .await;
    assert_eq!(granted, Ok("granted".into()));
    assert_eq!(polls, [Duration::ZERO, SECOND, SECOND * 5 / 2, SECOND * 7 / 2]);
}

#[tokio::test(start_paused = true)]
async fn an_approval_nobody_gives_expires_after_two_minutes_and_a_broken_answer_ends_it_at_once() {
    let (approval, started) = (fresh(), Instant::now());
    let mut polls = 0;
    let pending = approval
        .wait(|| {
            polls += 1;
            async { Ok(None) }
        })
        .await;
    assert_eq!(
        (pending, started.elapsed(), polls),
        (Err(Unapproved::Expired), Duration::from_mins(2), 120)
    );
    let broken = fresh().wait(|| async { Err(Fault::Malformed("token".into())) }).await;
    assert_matches!(broken, Err(Unapproved::Failed(failure)) if failure.reason == FailureReason::ProtocolError);
}

#[tokio::test(start_paused = true)]
async fn network_failures_last_until_the_deadline_which_reports_one_unless_the_last_poll_recovered() {
    let (approval, started) = (fresh(), Instant::now());
    let mut polls = 0;
    let failed = approval
        .wait(|| {
            polls += 1;
            let fault = Fault::Lost(format!("reset {polls}"));
            async { Err(fault) }
        })
        .await;
    assert_eq!(started.elapsed(), Duration::from_mins(2));
    match failed {
        Err(Unapproved::Failed(failure)) => assert_eq!(failure.reason, FailureReason::ConnectionLost),
        other => panic!("{other:?}"),
    }
    let mut polls = 0;
    let recovered = fresh()
        .wait(|| {
            polls += 1;
            let recovered = polls == 120;
            async move { if recovered { Ok(None) } else { Err(Fault::Lost("reset".into())) } }
        })
        .await;
    assert_eq!(recovered, Err(Unapproved::Expired));
}
