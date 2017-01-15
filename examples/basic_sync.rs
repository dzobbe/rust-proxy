extern crate meter_proxy;

use meter_proxy::proxy::sync::SyncMeterProxy;
use std::thread;
use std::time::Duration;

fn main() {
    println!("Starting Proxy");
    let meter_proxy = SyncMeterProxy::new("127.0.0.1".to_string(),
                                          12347,
                                          "127.0.0.1".to_string(),
                                          12349);

    let meter_proxy_c = meter_proxy.clone();
    let child_proxy = thread::spawn(move || {
        meter_proxy_c.start();
    });

    let mut n = 0;
    let sleep_time = 2000;
    while n < 100 {
        n += 1;

        // Do something
        thread::sleep(Duration::from_millis(sleep_time));

        println!("The measured latency 'till now: {:.3} ms",
                 meter_proxy.get_latency_ms());
        println!("The measured throughput 'till now: {:.3}",
                 meter_proxy.get_num_bytes_rcvd() as f64 / (n * sleep_time) as f64);
    }

    meter_proxy.stop_and_reset();
    let _ = child_proxy.join();

}
