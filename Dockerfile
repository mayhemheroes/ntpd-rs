FROM rust as builder

ADD . /ntpd-rs
WORKDIR /ntpd-rs/fuzz

RUN rustup toolchain add nightly
RUN rustup default nightly
RUN cargo +nightly install -f cargo-fuzz

RUN cargo fuzz build

FROM ubuntu:20.04

COPY --from=builder /ntpd-rs/fuzz/target/x86_64-unknown-linux-gnu/release/packet_parsing_sound / 
COPY --from=builder /ntpd-rs/fuzz/target/x86_64-unknown-linux-gnu/release/duration_from_float /
COPY --from=builder /ntpd-rs/fuzz/target/x86_64-unknown-linux-gnu/release/interval_finding /
COPY --from=builder /ntpd-rs/fuzz/target/x86_64-unknown-linux-gnu/release/ipfilter /
COPY --from=builder /ntpd-rs/fuzz/target/x86_64-unknown-linux-gnu/release/record_encode_decode /
COPY --from=builder /ntpd-rs/fuzz/target/x86_64-unknown-linux-gnu/release/tuple_from_packet /