The next step will be to convert this svelte + tailwind repo into a monorepo, where the server code lives next to the frontend browser side client (the current svelte project), and also where in the monorepo will then but the docker stuff which will then later be needed to build a full image that has both the server and the finsihed svelte client build, what is a sensible repository structure for all of this, also what if we might later decide, instead of the go server lets build a rust server, for even better performance, where would this one live in the monorepo. How do we ensure type saftey on the the apis, or do we just hardcode in every language?

for the server we will for now build a golang server, using quic-go and webtransport-go (both from the quic-go project), we choose go because we can now build a fully functioning server that can serve both http1.1 http2 and http3 (needed for webtransport).


So we will need to define the entire API for our tool, so we will have a normal preflight api to get some information about the server, its capabilities and what not (everything should be documented or in the code, what is needed here!). Then we will need to have mutliple endpoints for latency measurements, so one websocket and one webtransport (unreliable datagram, so we can use it also for packet loss measurement). Then for the upload downlaod endpoints we will need one that is just xhr streams and then then one that is a unidirectional webstransport stream. (all have to be well defined and clear so that the svelte client can then decide what backend it wants to use for what (so either all webtransport, or webtransport only for latency, but throughput xhr or whatever)) We need to define some kind off protocol which runs on the webtransport and websocket message bus, so maybe instead of json, something much faster, like keyword + number pattern or similar. (must be well defined, also some HI, ECHO, and then init and then timestamp or somthing for the latency meassages) On the topic of latency measurements, idealy the client would send a ping with a certain timestamp and id, and then the server will echo with pong and the id and a server timestamp, and then the client when receiving can check the id, clear it from its send pool, and send the next one right away, so we get this instant chain, on receive->send back -> on receive -> send back -> on receive -> send back, so we get absurtley high precision latency measurements with thousands of samples (this works well when kept lightweight, I tested it on websocket and unreliable webtransports, but off course we need to somehow keep track of it, no make anything overflow, when stuff gets dropped and so on, so it needs a bit of engineering) Then for the download endpoint, we need a super fast RNG (I attached a rust implementation of a very fast one so maybe recode in go) that can create huge ammounts of data, so maybe create buffer blocks that can then be send over and over again (with making sure any cache is invalidated, so some cache buster is needed). but the program must be made in a way that if we request gigabytes per second, it still keeps a minimal memory footprint so never fill the ram with the ammount requested, instead feed on the fly into the stream. So the client will request a certain size with ?bytes=xx and then the server will deliver on that endpoint till its done or aborted! and all of that with parallel streams. Then the upload endpoint is a bit trickier, because now we have to create huge ammounts of data in the browser in js this is hard, so I made the rust version to compile to webassembly and I tested it and could reliably get data rates from over 40 gbits of data throughput in the browser. Also maybe split this throughput and latency measurements into some kind off external web worker, as they will need a lot of cpu for all the data and requests, which might lead to the main thread comming to a halt! But rememebr everythin must performance otpimized, so the whole data, chain end to end, so maybe make the data never actually leave the worker, and just report back what it received or has send, and then somehow combine all the bytes and times from all the workers used for the ammount of parallel streams. 


The biggest question mark for me now is just how to make a single server that supports all http1.1, http2, http3, and then serve endpoints over the protocols, where some things only work over some protocols, but when in the browser having http3 conncetion over alt-svc header, then everything is quic anyway, so cant use the performance from http1.1 streams. or same for http2 connections, then the xhrstreams will be much faster over http1.1 as you can have multiple seperate real tcp connections and not like http2 where everything gets mutliplexed over a single one, so you do not get the performance benefit off mutiple streams. or then issue when you want to measure packet loss, then doing it with tcp based stuff will never work, because you do not get packet loss there I think, and then when you use webtransport unreliable datagram this is great and all and you can check how many of the latency pings never came back, but then you are required to have a http3 connection again, and when having to fallback to a lower standard, we can no longer masure packet loss, so I need a clear concept on what we should do about this, because this is all a bit complicated to be honest? 

so maybe have a different url for each? but that will get annoying for certificates, but might just be the best way forward, but then on which url serve what, havbe the webserver for the static html,js,css from the svelte client on all of them or just on one of them? What if users start using reverse proxies, will the user then need to forward all 3 ones completly seperatley, when later deploying as a docker container, every http generation on a different port, or what, I just do not know what is best, thats why we need this planning phase, so answer all the question marks! 


I also added to the repo 3 demos, on how to implement a high performance upload, download, ping worker thing for webbrowsers, these are thoroughly tested, but off course only cover a small scope of our project.


So what is very very important to remeber this tool in the end must only use tiny ammounts of ressources while providing enormous data streams (I already did something similar in rust and it was possible to have a program with 20MB ram usage that can deliver stable 40Gbps streams at 10% cpu) so this is definetly possible, its just a matter of engineering disipline, and asking at every hot path, how data flows, how memory is allocated, and how stream pressure is optimized, without creating huge buffers, also taking into consideration how the linux kernel deals with udp and tcp, how go and also quic-go do stuff on top of it (quic is often userspace and also userspace cryptop, I think), Thats the next issue, http1.1 can serve http and https, while newer http versions require tls, and tls is of course needed in modern browser stuff, but then you will definetly experience slow downs, so plain http1.1 without tls should always be possible, if wanted by the user.

So maybe build the application in a way, where the default case is just http1.1 and then if the user really wants to he can configure fancy stuff like webtransports, just for the sake of testing and benchmarking the web standards like quic, and how fast they are compared to the gold old http 1.1. 

As you might have realized by now, there are so many variables here, and possible changes, so the architecture for the server must be extremly modular and expandable, because almost anything might change in the future!





Use websearch to get the latest functionality of quic-go and webtransport-go, also to get the newest dependencies run the init commands and what not to generate the configs (instead of manually just writing the dependency files, also use websearch to get the latest apis of the packages) of the go project, or rust project if you want to put the wasm thing also somewhere in there, so it can then also be bundled into the dist folder. 


**Control Message Format for websocket and webtransport maybe:**
```
SIZE,<bytes>
PING,<uuid>
PONG,<identifier>;TIME,<unix-timestamp-nanos>
```
Where `<bytes>` is the requested download size in bytes.
but uuid must be extremly quickly calculated, no CPU overhead what so ever

what the ping endpoint needs to be caple of:

Optimized for minimal latency in all szenarios:
- **Local network**: 0.1-0.5ms ; thousands of requests per second
- **Same datacenter**: 1-5ms  h; unders of requests
- **Regional**: 10-50ms 
- **Intercontinental**: 100-300ms


RNG (Rust implementation) that can compile to wasm:


use wasm_bindgen::prelude::*;
use core::arch::wasm32::*;

#[wasm_bindgen]
pub struct ScrambledCounterRng {
    state: v128, // two lanes of u64
    inc: v128,   // two lanes of u64
}

#[wasm_bindgen]
impl ScrambledCounterRng {
    #[wasm_bindgen(constructor)]
    pub fn new(seed: u64, inc: u64) -> ScrambledCounterRng {
        let seed1 = seed;
        let seed2 = seed.wrapping_add(inc);
        let state = u64x2(seed1, seed2);
        let inc_vec = u64x2(inc, inc);
        ScrambledCounterRng { state, inc: inc_vec }
    }

    #[inline(always)]
    unsafe fn xorshift64star_vec(mut x: v128) -> v128 {
        // x ^= x >> 12
        x = v128_xor(x, u64x2_shr(x, 12));
        // x ^= x << 25
        x = v128_xor(x, u64x2_shl(x, 25));
        // x ^= x >> 27
        x = v128_xor(x, u64x2_shr(x, 27));

        // Scalar multiply per lane (fast)
        let lo0 = u64x2_extract_lane::<0>(x);
        let lo1 = u64x2_extract_lane::<1>(x);
        u64x2(
            lo0.wrapping_mul(0x2545F4914F6CDD1D),
            lo1.wrapping_mul(0x2545F4914F6CDD1D),
        )
    }

    #[inline(always)]
    fn scalar_xorshift64star(mut x: u64) -> u64 {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    #[wasm_bindgen]
    pub fn fill_bytes(&mut self, buf: &mut [u8]) {
        let len = buf.len();
        let mut i = 0;

        unsafe {
            // Main SIMD loop
            while i + 16 <= len {
                let v = self.state;
                self.state = u64x2_add(self.state, self.inc);
                let rnd = Self::xorshift64star_vec(v);
                v128_store(buf.as_mut_ptr().add(i) as *mut v128, rnd);
                i += 16;
            }

        // Final tail (handles both 8-byte and <8-byte cases)
        if i < len {
            let remaining = len - i;
            let mut tmp = [0u8; 16];

            if remaining >= 8 {
                // Generate two u64s into tmp
                let v = self.state;
                self.state = u64x2_add(self.state, self.inc);
                let rnd = Self::xorshift64star_vec(v);
                v128_store(tmp.as_mut_ptr() as *mut v128, rnd);
            } else {
                // Generate one u64 into tmp
                let lane0 = u64x2_extract_lane::<0>(self.state);
                let inc0 = u64x2_extract_lane::<0>(self.inc);
                self.state = u64x2(lane0.wrapping_add(inc0), 0);
                tmp[..8].copy_from_slice(&Self::scalar_xorshift64star(lane0).to_le_bytes());
            }
            buf[i..].copy_from_slice(&tmp[..remaining]);
        }
    }
    }
}


And here is a rust download endpoint I have build which was pretty fast


//! # Download API Endpoint
//!
//! High-throughput download test endpoint using WebTransport unidirectional
//! streams, optimized with a simple, tight RNG and pointer-based fills.

use std::time::Instant;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::time::{sleep, Duration};
use tracing::info;

use crate::api::{
    session::ApiSession,
    types::{ApiResult, EndpointCapabilities, WebTransportEndpoint},
};

use rand::Rng;
use rand_core::{Error as RandError, RngCore};

#[inline(always)]
fn xorshift64star(mut x: u64) -> u64 {
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

pub struct ScrambledCounterRng {
    state: u64,
    inc: u64,
}

impl ScrambledCounterRng {
    #[inline(always)]
    pub const fn new(seed: u64, inc: u64) -> Self {
        Self { state: seed, inc }
    }

    #[inline(always)]
    fn next_u64_inline(&mut self) -> u64 {
        let old = self.state;
        self.state = old.wrapping_add(self.inc);
        xorshift64star(old)
    }
}

impl RngCore for ScrambledCounterRng {
    #[inline(always)]
    fn next_u32(&mut self) -> u32 {
        self.next_u64_inline() as u32
    }

    #[inline(always)]
    fn next_u64(&mut self) -> u64 {
        self.next_u64_inline()
    }

    // Tight, pointer-based filler. Native-endian is fine for random payloads.
    #[inline(always)]
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        let mut remaining = dest.len();
        let mut ptr = dest.as_mut_ptr();

        unsafe {
            // Main loop: write 8 bytes at a time
            while remaining >= 8 {
                let v = self.next_u64_inline();
                std::ptr::write_unaligned(ptr as *mut u64, v);
                ptr = ptr.add(8);
                remaining -= 8;
            }

            // Tail: copy remaining bytes from the u64 on the stack
            if remaining > 0 {
                let v = self.next_u64_inline();
                let v_ptr = &v as *const u64 as *const u8;
                std::ptr::copy_nonoverlapping(v_ptr, ptr, remaining);
            }
        }
    }

    #[inline(always)]
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), RandError> {
        self.fill_bytes(dest);
        Ok(())
    }
}

#[derive(Debug)]
pub struct DownloadEndpoint;

impl DownloadEndpoint {
    pub fn new() -> Self {
        Self
    }

    async fn handle_download(&self, mut session: ApiSession) -> ApiResult<()> {
        info!(
            "Download session started for path: {}",
            session.endpoint_path()
        );

        let size: usize = session
            .context()
            .query_params
            .get("size")
            .and_then(|s| s.parse().ok())
            .unwrap_or(10_000_000); // default 10 MB

        // Allow tuning chunk size via query param, else use a solid default.
        let chunk_size: usize = session
            .context()
            .query_params
            .get("chunk")
            .and_then(|s| s.parse().ok())
            .unwrap_or(2 * 1024 * 1024); // 2 MiB

        info!("Streaming {} bytes, chunk {}", size, chunk_size);

        // Open unidirectional stream and start sending data
        let mut send_stream = session.open_uni().await?;

        // Pre-allocate buffer once
        let mut buf = vec![0u8; chunk_size];

        // Single RNG instance; fast and branchless
        let seed: u64 = rand::rng().random();
        let mut rng = ScrambledCounterRng::new(seed, 1);

        let mut remaining = size;
        let start_send = Instant::now();

        while remaining > 0 {
            let to_write = remaining.min(chunk_size);
            rng.fill_bytes(&mut buf[..to_write]);
            send_stream.write_all(&buf[..to_write]).await?;
            remaining -= to_write;
        }

        send_stream.shutdown().await?;
        // Small grace; reduce to 10ms or remove entirely
        sleep(Duration::from_millis(10)).await;

        let send_time = start_send.elapsed();
        let bps = size as f64 / send_time.as_secs_f64();
        let mbps = bps * 8.0 / 1_000_000.0;

        info!(
            "Download completed: {} bytes in {:?} ({:.2} Mbps)",
            size, send_time, mbps
        );

        Ok(())
    }
}

#[async_trait]
impl WebTransportEndpoint for DownloadEndpoint {
    fn capabilities(&self) -> EndpointCapabilities {
        EndpointCapabilities::streams_only()
    }

    async fn handle_session(&self, session: ApiSession) -> ApiResult<()> {
        self.handle_download(session).await
    }
}

