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

/// The calling thread's nice value, from Linux's `/proc`.
#[cfg(target_os = "linux")]
fn nice() -> i64 {
    let stat = std::fs::read_to_string("/proc/thread-self/stat").unwrap();
    let fields: Vec<_> = stat.rsplit_once(')').unwrap().1.split_whitespace().collect();
    fields[16].parse().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_probing_pool_runs_latency_on_its_own_thread_above_the_pinned_ones() {
    let pool = Pool::probing(&Handle::current()).unwrap();
    assert_eq!(pool.runtimes().len(), 2);
    assert_eq!(thread_of(pool.latency()).await, "gm-latency");
    #[cfg(target_os = "linux")]
    {
        let nice_of = async |runtime: Handle| runtime.spawn(async { nice() }).await.unwrap();
        let (latency, pinned) = (nice_of(pool.latency()).await, nice_of(pool.next()).await);
        // Linux caps nice at 19.
        assert_eq!(pinned, (latency + 5).min(19), "pinned threads run five steps below the latency runtime");
    }
}
