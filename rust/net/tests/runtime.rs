//! Connections pinned to the pool's runtimes, one thread each, in turn.
use graphite_meter_net::Pool;

async fn thread_of(runtime: tokio::runtime::Handle) -> String {
    let name = runtime.spawn(async { std::thread::current().name().map(str::to_owned) });
    name.await.unwrap().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn each_worker_gets_a_runtime_on_its_own_thread_picked_in_turn() {
    let pool = Pool::new().unwrap();
    assert_eq!(pool.runtimes().len(), 3);
    let mut threads = Vec::new();
    for _ in 0..4 {
        threads.push(thread_of(pool.next()).await);
    }
    assert_eq!(threads, ["gm-worker-0", "gm-worker-1", "gm-worker-2", "gm-worker-0"]);
    for runtime in pool.runtimes() {
        assert_eq!(runtime.runtime_flavor(), tokio::runtime::RuntimeFlavor::CurrentThread);
    }
}

#[tokio::test]
async fn under_a_current_thread_runtime_the_caller_s_runtime_serves() {
    let pool = Pool::new().unwrap();
    assert!(pool.runtimes().is_empty());
    let caller = std::thread::current().name().map(str::to_owned).unwrap_or_default();
    assert_eq!(thread_of(pool.next()).await, caller);
}
