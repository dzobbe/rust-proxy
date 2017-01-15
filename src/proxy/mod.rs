/// Copyright 2016 Giovanni Mazzeo. See the COPYRIGHT
/// file at the top-level directory of this distribution and at
/// http://rust-lang.org/COPYRIGHT.
///
/// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
/// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
/// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
/// option. This file may not be copied, modified, or distributed
/// except according to those terms.
/// ///////////////////////////////////////////////////////////////////////////

use time;
use libc;
use ansi_term::Colour::{Red, Yellow};
use std::net::{TcpListener, TcpStream, Shutdown, SocketAddr, IpAddr};
use std::sync::{Arc, Mutex, RwLock};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{thread, str};
use std::time::Duration;
use std::io::prelude::*;
use libc::setrlimit;
use std::collections::HashMap;
use std::sync::mpsc::{channel, Sender, Receiver};


/**
The MeterProxy is a proxy which interposes between two applications and measures
their latency and throughput (KB/s). The convention is the following:

SERVER <=====> (back) PROXY (front) <=====> CLIENT

Therefore, the proxy will listen on the front-side from requests of a client and will
forward the requests on the back-side to the server
**/
/// /////////////////////////////////////////////////////////////////////
/// /////////////////////////////////////////////////////////////////////
/**
Definition of a Shared Counter for the THROUGHPUT evaluation and a
Time Vector for the LATENCY evaluation. Each thread will put its measurement.
**/
struct SharedCounter(Arc<Mutex<usize>>);
impl SharedCounter {
    fn new() -> Self {
        SharedCounter(Arc::new(Mutex::new(0)))
    }
    fn increment(&self, quantity: usize) {
        let mut counter = self.0.lock().unwrap();
        *counter = *counter + quantity;
    }
    fn get(&self) -> usize {
        let counter = self.0.lock().unwrap();
        *counter
    }

    fn reset(&self) {
        let mut counter = self.0.lock().unwrap();
        *counter = 0;
    }
}

struct SharedTimeVec(Arc<Mutex<Vec<u64>>>);
impl SharedTimeVec {
    fn new() -> Self {
        SharedTimeVec(Arc::new(Mutex::new(Vec::new())))
    }

    fn insert(&self, value: u64) {
        let mut time_vec = self.0.lock().unwrap();
        time_vec.push(value);
    }

    fn get_avg_value(&self) -> f64 {
        let mut time_vec = self.0.lock().unwrap();
        let sum: u64 = time_vec.iter().sum();
        return sum as f64 / time_vec.len() as f64;
    }

    fn reset(&self) {
        let mut time_vec = self.0.lock().unwrap();
        time_vec.clear();
    }
}

lazy_static! {
    static ref TIME_TABLE: SharedTimeVec   = {SharedTimeVec::new()};
    static ref NUM_BYTES : SharedCounter   = {SharedCounter::new()};

    static ref ERROR: Arc<Mutex<bool>>	   = Arc::new(Mutex::new(false));
}

/// /////////////////////////////////////////////////////////////////////
/// /////////////////////////////////////////////////////////////////////


#[derive(Clone)]
pub struct MeterProxy {
    pub back_address: String,
    pub back_port: u16,
    pub front_address: String,
    pub front_port: u16,
    pub reset_lock_flag: Arc<RwLock<bool>>,
}


impl MeterProxy {
    pub fn new(b_addr: String, b_port: u16, f_addr: String, f_port: u16) -> MeterProxy {
        MeterProxy {
            back_address: b_addr,
            back_port: b_port,
            front_address: f_addr,
            front_port: f_port,
            reset_lock_flag: Arc::new(RwLock::new(false)),
        }
    }


    // Start the Proxy
    pub fn start(&self) {
        // Increase file descriptor resources limits (this avoids  the risk of exception: "Too many open files (os error 24)")
        let rlim = libc::rlimit {
            rlim_cur: 4096,
            rlim_max: 4096,
        };
        unsafe {
            libc::setrlimit(libc::RLIMIT_NOFILE, &rlim);
        }

        let targ_addr: IpAddr = self.front_address
            .parse()
            .expect("Unable to parse FRONT-side Address");

        // Start the proxy listening on the FRONT-side
        let acceptor = TcpListener::bind((targ_addr, self.front_port)).unwrap();
        let mut children = vec![];

        for stream in acceptor.incoming() {

            let reset_lock_flag_c = self.reset_lock_flag.clone();
            let back_addr_c = self.clone().back_address;
            let back_port_c = self.back_port;

            if *reset_lock_flag_c.read().unwrap() == true {
                // Reset Flag raised: This is a way to interrupt from an external thread the ProxyServer
                break;
            }

            match stream {
                Err(e) => println!("Strange connection broken: {}", e),
                Ok(stream) => {
                    children.push(thread::spawn(move || {
                        // connection succeeded
                        let mut stream_c = stream.try_clone().unwrap();
                        let stream_c2 = stream.try_clone().unwrap();
                        stream_c2.set_read_timeout(Some(Duration::new(3, 0)));

                        // Start a pipe for every connection coming from the client
                        MeterProxy::start_pipe(stream_c, back_addr_c, back_port_c);
                        drop(stream);

                    }));

                }
            }
        }
        for child in children {
            // Wait for the thread to finish. Returns a result.
            let _ = child.join();
        }
        drop(acceptor);
        return;
    }


    /**
	Stop the proxy server and clean resources
	**/
    pub fn stop_and_reset(&self) {
        *self.reset_lock_flag.write().unwrap() = true;
        NUM_BYTES.reset();
        TIME_TABLE.reset();

        // Spurious connection needed to break the proxy server loop
        let targ_addr: IpAddr = self.front_address
            .parse()
            .expect("Unable to parse FRONT-side Address");
        TcpStream::connect((targ_addr, self.front_port));
    }


    pub fn get_num_bytes_rcvd(&self) -> usize {
        return NUM_BYTES.get();
    }

    pub fn get_latency_ms(&self) -> f64 {
        return TIME_TABLE.get_avg_value() / 1000000.0f64;
    }

    fn start_pipe(front: TcpStream, target_addr: String, target_port: u16) {

        let targ_addr: IpAddr = target_addr.parse()
            .expect("Unable to parse BACK-side Address");
        let mut back = match TcpStream::connect((targ_addr, target_port)) {
            Err(e) => {

                let mut err = ERROR.lock().unwrap();
                if *err == false {
                    println!("{} Unable to connect to the Target Application. Maybe a bad \
                              configuration: {}",
                             Red.paint("*****ERROR***** --> "),
                             e);
                };
                *err = true;
                front.shutdown(Shutdown::Both);
                drop(front);
                return;
            }
            Ok(b) => b,
        };



        let front_c = front.try_clone().unwrap();
        let back_c = back.try_clone().unwrap();

        let timedOut = Arc::new(AtomicBool::new(false));
        let timedOut_c = timedOut.clone();


        let latency_mutex: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));
        let (tx, rx) = channel();
        let latency_mutex_c = latency_mutex.clone();


        thread::spawn(move || {
            MeterProxy::keep_copying_bench_2_targ(front, back, timedOut, latency_mutex, tx);
        });

        thread::spawn(move || {
            MeterProxy::keep_copying_targ_2_bench(back_c, front_c, timedOut_c, latency_mutex_c, rx);
        });


    }

    /**
	Pipe BACK(Server)<======FRONT(Client)
	**/
    fn keep_copying_bench_2_targ(mut front: TcpStream,
                                 mut back: TcpStream,
                                 timedOut: Arc<AtomicBool>,
                                 time_mutex: Arc<Mutex<u64>>,
                                 tx: Sender<u8>) {

        front.set_read_timeout(Some(Duration::new(1000, 0)));
        let mut buf = [0; 1024];


        loop {

            let read = match front.read(&mut buf) {
                Err(ref err) => {
                    let other = timedOut.swap(true, Ordering::AcqRel);
                    if other {
                        // the other side also timed-out / errored, so lets go
                        drop(front);
                        drop(back);
                        return;
                    }
                    // normal errors, just stop
                    front.shutdown(Shutdown::Both);
                    back.shutdown(Shutdown::Both);
                    // normal errors, just stop
                    drop(front);
                    drop(back);
                    return; // normal errors, stop
                }
                Ok(r) => r,
            };


            let mut start_time = time_mutex.lock().unwrap();
            *start_time = time::precise_time_ns();

            timedOut.store(false, Ordering::Release);
            match back.write(&buf[0..read]) {
                Err(..) => {
                    timedOut.store(true, Ordering::Release);
                    // normal errors, just stop
                    front.shutdown(Shutdown::Both);
                    back.shutdown(Shutdown::Both);
                    drop(front);
                    drop(back);
                    return;
                }
                Ok(..) => (),
            };

            tx.send(1).unwrap();
        }

    }

    /**
	Pipe BACK(Server)======>FRONT(Client)
	**/
    fn keep_copying_targ_2_bench(mut back: TcpStream,
                                 mut front: TcpStream,
                                 timedOut: Arc<AtomicBool>,
                                 time_mutex: Arc<Mutex<u64>>,
                                 rx: Receiver<u8>) {

        back.set_read_timeout(Some(Duration::new(1000, 0)));
        let mut buf = [0; 1024];

        // SeqNumber for latency measuring
        let mut seq_number = 0;

        loop {

            let read = match back.read(&mut buf) {
                Err(ref err) => {
                    let other = timedOut.swap(true, Ordering::AcqRel);
                    if other {
                        // the other side also timed-out / errored, so lets go
                        drop(back);
                        drop(front);
                        return;
                    }
                    // normal errors, just stop
                    front.shutdown(Shutdown::Both);
                    back.shutdown(Shutdown::Both);
                    drop(back);
                    drop(front);

                    return; // normal errors, stop
                }
                Ok(r) => r,
            };

            match rx.try_recv() {
                Ok(r) => {
                    let res = *(time_mutex.lock().unwrap());
                    TIME_TABLE.insert(time::precise_time_ns() - res);
                }
                RecvError => {}
            };

            // Increment the number of bytes read counter
            NUM_BYTES.increment(read);


            timedOut.store(false, Ordering::Release);
            match front.write(&buf[0..read]) {
                Err(..) => {
                    timedOut.store(true, Ordering::Release);
                    // normal errors, just stop
                    front.shutdown(Shutdown::Both);
                    back.shutdown(Shutdown::Both);
                    drop(back);
                    drop(front);
                    return;
                }
                Ok(..) => (),
            };


        }

        drop(back);
        drop(front);


    }
}
