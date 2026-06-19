The next step will be to convert this svelte + tailwind repo into a monorepo, where the server code lives next to the frontend browser side client (the current svelte project)

for the server we will for now build a golang server, using quic-go and webtransport-go (both from the quic-go project), we choose go because we can now build a fully function server that can serve both http1.1 http2 and http3 (needed for webtransport).


So we will need to define the entire API for our tool, so we will have a normal preflight api to get some information about the server, its capabilities and what not (everything should be documented or in the code, what is needed here!). Then we will need to have mutliple endpoints for latency measurements, so one websocket and one webtransport (unreliable datagram, so we can use it also for packet loss measurement). Then for the upload downlaod endpoints we will need one that is just xhr streams and then then one that is a unidirectional webstransport stream. (all have to be well defined and clear so that the svelte client can then decide what backend it wants to use for what (so either all webtransport, or webtransport only for latency, but throughput xhr or whatever)) We need to define some kind off protocol which runs on the webtransport and websocket message bus, so maybe instead of json, something much faster, like keyword + number pattern or similar. (must be well defined, also some HI, ECHO, and then init and then timestamp or somthing for the latency meassages) On the topic of latency measurements, idealy the client would send a ping with a certain timestamp and id, and then the server will echo with pong and the id and a server timestamp, and then the client when receiving can check the id, clear it from its send pool, and send the next one right away, so we get this instant chain, on receive->send back -> on receive -> send back -> on receive -> send back, so we get absurtley high precision latency measurements with thousands of samples (this works well when kept lightweight, I tested it on websocket and unreliable webtransports, but off course we need to somehow keep track of it, no make anything overflow, when stuff gets dropped and so on, so it needs a bit of engineering) Then for the download endpoint, we need a super fast RNG (I attached a rust implementation of a very fast one so maybe recode in go) that can create huge ammounts of data, so maybe create buffer blocks that can then be send over and over again (with making sure any cache is invalidated, so some cache buster is needed). but the program must be made in a way that if we request gigabytes per second, it still keeps a minimal memory footprint so never fill the ram with the ammount requested, instead feed on the fly into the stream. So the client will request a certain size with ?bytes=xx and then the server will deliver on that endpoint till its done or aborted! and all of that with parallel streams. Then the upload endpoint is a bit trickier, because now we have to create huge ammounts of data in the browser in js this is hard, so I made the rust version to compile to webassembly and I tested it and could reliably get data rates from over 40 gbits of data throughput in the browser. Also maybe split this throughput and latency measurements into some kind off external web worker, as they will need a lot of cpu for all the data and requests, which might lead to the main thread comming to a halt! But rememebr everythin must performance otpimized, so the whole data, chain end to end, so maybe make the data never actually leave the worker, and just report back what it received or has send, and then somehow combine all the bytes and times from all the workers used for the ammount of parallel streams. 


The biggest question mark for me now is just how to make a single server that supports all http1.1, http2, http3, and then serve endpoints over the protocols, where some things only work over some protocols, but when in the browser having http3 conncetion over alt-svc header, then everything is quic anyway, so cant use the performance from http1.1 streams. or same for http2 connections, then the xhrstreams will be much faster over http1.1 as you can have multiple seperate real tcp connections and not like http2 where everything gets mutliplexed over a single one, so you do not get the performance benefit off mutiple streams. or then issue when you want to measure packet loss, then doing it with tcp based stuff will never work, because you do not get packet loss there I think, and then when you use webtransport unreliable datagram this is great and all and you can check how many of the latency pings never came back, but then you are required to have a http3 connection again, and when having to fallback to a lower standard, we can no longer masure packet loss, so I need a clear concept on what we should do about this, because this is all a bit complicated to be honest? 


Use websearch to get the latest functionality of quic-go and webtransport-go






RNG (Rust implementation):


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