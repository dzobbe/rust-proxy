# rust-proxy [![Crates.io](https://img.shields.io/crates/v/meterproxy.svg)](https://crates.io/crates/meterproxy)
This is a meter proxy implemented in Rust, which interposes between a client and a server and measures the latency and the throughput. An asychronous version will come soon.

## Requirements
Of course, you will need Rust installed. If you haven't already, get it here: [rust-lang.org](https://www.rust-lang.org). Also you need [Cargo](https://crates.io) to easily compile. The rustc compiler version required is the 1.15.0-nightly.

## Usage
To use the proxy, add this to your `Cargo.toml`:

```toml
[dependencies]
meterproxy = "0.1.0"
```

## Example Meter Proxy Usage

```rust
extern crate meterproxy;
use meterproxy::MeterProxy;
use std::thread;
use std::time::Duration;

fn main() {
    println!("Starting Proxy");
    let meter_proxy=MeterProxy::MeterProxy::new("127.0.0.1".to_string(), 12347,"127.0.0.1".to_string(),12349); 

    let meter_proxy_c=meter_proxy.clone();
    let child_proxy = thread::spawn(move || {
        meter_proxy_c.start();
    });

    let mut n=0;
    let sleep_time=2000;
    while n<100{
        n += 1;

        //Do something
        thread::sleep(Duration::from_millis(sleep_time));

        println!("The measured latency 'till now: {:.3} ms",meter_proxy.get_latency_ms());
        println!("The measured throughput 'till now: {:.3}",meter_proxy.get_num_bytes_rcvd() as f64/(n*sleep_time) as f64);
    }

    meter_proxy.stop_and_reset();
    let _ = child_proxy.join();

}
```




