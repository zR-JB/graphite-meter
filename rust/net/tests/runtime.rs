//! Connections pinned to the pool's runtimes, one thread each, in turn.
use graphite_meter_net::Pool;
use tokio::runtime::{Handle, RuntimeFlavor};

async fn thread_of(runtime: Handle) -> String {
    let name = runtime.spawn(async { std::thread::current().name().map(str::to_owned) });
    name.await.unwrap().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn each_worker_gets_a_runtime_on_its_own_thread_picked_in_turn() {
    let pool = Pool::beside(&Handle::current()).unwrap();
    assert_eq!(pool.runtimes().len(), 3);
    let mut threads = Vec::new();
    for _ in 0..4 {
        threads.push(thread_of(pool.next()).await);
    }
    assert_eq!(threads, ["gm-worker-0", "gm-worker-1", "gm-worker-2", "gm-worker-0"]);
    for runtime in pool.runtimes() {
        assert_eq!(runtime.runtime_flavor(), RuntimeFlavor::CurrentThread);
    }
}

#[tokio::test]
async fn a_current_thread_runtime_is_refused_and_an_inline_pool_serves_on_the_caller_s() {
    let refused = Pool::beside(&Handle::current()).err().unwrap();
    assert_eq!(refused.kind(), std::io::ErrorKind::Unsupported);
    let pool = Pool::inline();
    assert!(pool.runtimes().is_empty());
    let caller = std::thread::current().name().map(str::to_owned).unwrap_or_default();
    assert_eq!(thread_of(pool.next()).await, caller);
}
