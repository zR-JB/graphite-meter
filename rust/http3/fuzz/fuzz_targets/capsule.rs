#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| graphite_meter_http3::fuzz::capsules(data));
