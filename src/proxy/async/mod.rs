use std::panic;
use std::cell::RefCell;
use std::rc::Rc;
use std::env;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, IpAddr};
use std::net::Shutdown;
use std::str;
use std::time::Duration;
use libc;
use futures;
use futures::{Future, Poll, Async};
use futures::stream::Stream;
use futures_cpupool::CpuPool;
use tokio_core::reactor::{Core, Handle, Timeout};
use tokio_core::net::{TcpStream, TcpListener};
use tokio_core::io::{Io, read_exact, write_all, Window};
use std::sync::{Arc, Mutex, RwLock};


lazy_static! {
    static ref ERROR: Arc<Mutex<bool>>	= Arc::new(Mutex::new(false));
}

#[derive(Clone, Debug)]
pub struct AsyncMeterProxy {
    pub back_address: String,
    pub back_port: u16,
    pub front_address: String,
    pub front_port: u16,
    pub num_bytes: Arc<Mutex<f64>>,
    pub num_resp: Arc<Mutex<f64>>,
}


impl AsyncMeterProxy {
    pub fn new(b_addr: String, b_port: u16, f_addr: String, f_port: u16) -> AsyncMeterProxy {
        AsyncMeterProxy {
            back_address: b_addr,
            back_port: b_port,
            front_address: f_addr,
            front_port: f_port,
            num_bytes: Arc::new(Mutex::new(0.0)),
            num_resp: Arc::new(Mutex::new(0.0)),
        }
    }


    // Start the Proxy
    pub fn start(&self) {

        let rlim = libc::rlimit {
            rlim_cur: 4096,
            rlim_max: 4096,
        };
        unsafe {
            libc::setrlimit(libc::RLIMIT_NOFILE, &rlim);
        }

        let mut core = Core::new().unwrap();
        let mut lp = Core::new().unwrap();
        let pool = CpuPool::new(4);
    	let buffer = Rc::new(RefCell::new(vec![0; 64 * 1024]));
        let handle = lp.handle();


        let f_addr_c = self.front_address.clone();
        let b_addr_c = self.back_address.clone();

        let front_address =
            (f_addr_c + ":" + &self.front_port.to_string()).parse::<SocketAddr>().unwrap();
        let back_address =
            (b_addr_c + ":" + &self.back_port.to_string()).parse::<SocketAddr>().unwrap();

        let listener = TcpListener::bind(&front_address, &handle).unwrap();


        // Construct a future representing our server. This future processes all
        // incoming connections and spawns a new task for each client which will do
        // the proxy work.
        let clients = listener.incoming().map(move |(socket, addr)| {
            (Client {
                     buffer: buffer.clone(),
                     pool: pool.clone(),
                     handle: handle.clone(),
                     num_bytes: self.num_bytes.clone(),
                     num_resp: self.num_resp.clone(),
                 }
                 .serve(socket, back_address),
             addr)
        });
        let handle = lp.handle();
        let server = clients.for_each(|(client, addr)| {
            handle.spawn(client.then(move |res| {
                match res {
                    Ok((a, b)) => println!("proxied {}/{} bytes for {}", a, b, addr),
                    Err(e) => {;
                    }
                }
                futures::finished(())
            }));
            Ok(())
        });


        // Now that we've got our future ready to go, let's run it!
        //
        // This `run` method will return the resolution of the future itself, but
        // our `server` futures will resolve to `io::Result<()>`, so we just want to
        // assert that it didn't hit an error.
        lp.run(server).unwrap();
    }


    /**
	Reset the proxy server counter
	**/
    pub fn reset(&self) {
        {
            let mut n_bytes = self.num_bytes.lock().unwrap();
            *n_bytes = 0.0;
        }
        {
            let mut n_resp = self.num_resp.lock().unwrap();
            *n_resp = 0.0;
        }

    }


    pub fn get_num_kbytes_rcvd(&self) -> f64 {
        let n_bytes = self.num_bytes.lock().unwrap();
        return *n_bytes as f64 / 1024.0f64;
    }

    pub fn get_latency(&self) -> f64 {
        let n_resp = self.num_resp.lock().unwrap();
        return *n_resp as f64 / 1000000.0f64;
    }
}

// Data used to when processing a client to perform various operations over its
// lifetime.
struct Client {
    buffer: Rc<RefCell<Vec<u8>>>,
    pool: CpuPool,
    handle: Handle,
    num_bytes: Arc<Mutex<f64>>,
    num_resp: Arc<Mutex<f64>>,
}

impl Client {
    fn serve(self,
             front_socket: TcpStream,
             back_addr: SocketAddr)
             -> Box<Future<Item = (u64, u64), Error = io::Error>> {

        let pool = self.pool.clone();


        // Now that we've got a socket address to connect to, let's actually
        // create a connection to that socket!
        //
        // To do this, we use our `handle` field, a handle to the event loop, to
        // issue a connection to the address we've figured out we're going to
        // connect to. Note that this `tcp_connect` method itself returns a
        // future resolving to a `TcpStream`, representing how long it takes to
        // initiate a TCP connection to the remote.
        
        
        let handle = self.handle.clone();

        let pair = TcpStream::connect(&back_addr, &handle)
            .and_then(|back_socket| futures::lazy(|| Ok((back_socket, front_socket))));


        let buffer = self.buffer.clone();
        let n_bytes = self.num_bytes.clone();
        let n_resp = self.num_resp.clone();

        mybox(pair.and_then(|(back, front)| {
            let back = Rc::new(back);
            let front = Rc::new(front);

            let half1 = TransferFrontBack::new(back.clone(),
                                               front.clone(),
                                               buffer.clone(),
                                               n_bytes.clone(),
                                               n_resp.clone());
            let half2 = TransferBackFront::new(front, back, buffer, n_bytes, n_resp);
            half1.join(half2)
        }))
    }
}


fn mybox<F: Future + 'static>(f: F) -> Box<Future<Item = F::Item, Error = F::Error>> {
    Box::new(f)
}



/// A future representing reading all data from one side of a proxy connection
/// and writing it to another.
///
/// This future, unlike the handshake performed above, is implemented via a
/// custom implementation of the `Future` trait rather than with combinators.
/// This is intended to show off how the combinators are not all that can be
/// done with futures, but rather more custom (or optimized) implementations can
/// be implemented with just a trait impl!
struct TransferBackFront {
    // The two I/O objects we'll be reading.
    reader: Rc<TcpStream>,
    writer: Rc<TcpStream>,

    // The shared global buffer that all connections on our server are using.
    buf: Rc<RefCell<Vec<u8>>>,

    // The number of bytes we've written so far.
    amt: u64,
    num_bytes: Arc<Mutex<f64>>,
    num_resp: Arc<Mutex<f64>>,
}

impl TransferBackFront {
    fn new(reader: Rc<TcpStream>,
           writer: Rc<TcpStream>,
           buffer: Rc<RefCell<Vec<u8>>>,
           n_bytes: Arc<Mutex<f64>>,
           n_resp: Arc<Mutex<f64>>)
           -> TransferBackFront {

        TransferBackFront {
            reader: reader,
            writer: writer,
            buf: buffer,
            amt: 0,
            num_bytes: n_bytes,
            num_resp: n_resp,
        }
    }
}

// Here we implement the `Future` trait for `Transfer` directly. This does not
// use any combinators, and shows how you might implement it in custom
// situations if needed.
impl Future for TransferBackFront {
    // Our future resolves to the number of bytes transferred, or an I/O error
    // that happens during the connection, if any.
    type Item = u64;
    type Error = io::Error;


    fn poll(&mut self) -> Poll<u64, io::Error> {
        let mut buffer = self.buf.borrow_mut();


        // Here we loop over the two TCP halves, reading all data from one
        // connection and writing it to another. The crucial performance aspect
        // of this server, however, is that we wait until both the read half and
        // the write half are ready on the connection, allowing the buffer to
        // only be temporarily used in a small window for all connections.
        loop {
            let read_ready = self.reader.poll_read().is_ready();

            let write_ready = self.writer.poll_write().is_ready();
            if !read_ready || !write_ready {
                return Ok(Async::NotReady);
            }


            let n = try_nb!((&*self.reader).read(&mut buffer));
            if n == 0 {
                try!(self.writer.shutdown(Shutdown::Write));
                return Ok(self.amt.into());
            }

            self.amt += n as u64;


            // Unlike above, we don't handle `WouldBlock` specially, because
            // that would play into the logic mentioned above (tracking read
            // rates and write rates), so we just ferry along that error for
            // now.
            let m = try!((&*self.writer).write(&buffer[..n]));
            assert_eq!(n, m);
        }
    }
}

struct TransferFrontBack {
    // The two I/O objects we'll be reading.
    reader: Rc<TcpStream>,
    writer: Rc<TcpStream>,

    // The shared global buffer that all connections on our server are using.
    buf: Rc<RefCell<Vec<u8>>>,

    // The number of bytes we've written so far.
    amt: u64,

    num_bytes: Arc<Mutex<f64>>,
    num_resp: Arc<Mutex<f64>>,
}

impl TransferFrontBack {
    fn new(reader: Rc<TcpStream>,
           writer: Rc<TcpStream>,
           buffer: Rc<RefCell<Vec<u8>>>,
           n_bytes: Arc<Mutex<f64>>,
           n_resp: Arc<Mutex<f64>>)
           -> TransferFrontBack {
        TransferFrontBack {
            reader: reader,
            writer: writer,
            buf: buffer,
            amt: 0,
            num_bytes: n_bytes,
            num_resp: n_resp,
        }
    }
}


impl Future for TransferFrontBack {
    // Our future resolves to the number of bytes transferred, or an I/O error
    // that happens during the connection, if any.
    type Item = u64;
    type Error = io::Error;


    fn poll(&mut self) -> Poll<u64, io::Error> {
        let mut buffer = self.buf.borrow_mut();

        // Here we loop over the two TCP halves, reading all data from one
        // connection and writing it to another. The crucial performance aspect
        // of this server, however, is that we wait until both the read half and
        // the write half are ready on the connection, allowing the buffer to
        // only be temporarily used in a small window for all connections.
        loop {
            let read_ready = self.reader.poll_read().is_ready();

            let write_ready = self.writer.poll_write().is_ready();
            if !read_ready || !write_ready {
                return Ok(Async::NotReady);
            }


            let n = try_nb!((&*self.reader).read(&mut buffer));
            if n == 0 {
                try!(self.writer.shutdown(Shutdown::Write));
                return Ok(self.amt.into());
            }

            self.amt += n as u64;


            {
                let mut n_bytes = self.num_bytes.lock().unwrap();
                *n_bytes += n as f64;
            }

            { 
                let mut n_resp = self.num_resp.lock().unwrap();
                *n_resp += 1.0;
            }

            // Unlike above, we don't handle `WouldBlock` specially, because
            // that would play into the logic mentioned above (tracking read
            // rates and write rates), so we just ferry along that error for
            // now.
            let m = try!((&*self.writer).write(&buffer[..n]));
            assert_eq!(n, m);
        }
    }
}

fn other(desc: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Other, desc)
}
