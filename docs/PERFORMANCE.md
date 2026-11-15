# Performance

This document describes how performance is measured, the current results, and what limits throughput. The design choices behind these numbers are covered in [ARCHITECTURE.md](ARCHITECTURE.md).

## What limits throughput

For almost every user, **the mobile network is the bottleneck, not this program.** A USB 2.0 High-Speed link can carry roughly 35–40 MB/s of payload, and comparable userspace RNDIS drivers have been measured at about 300 Mbit/s. The tests below ran at 1.5–3 MB/s because that is what the phone's cellular connection delivered.

What the program itself controls, and what this document tracks, is **cost**: the CPU time spent per megabyte carried, and how often it wakes up when there's nothing to do.

## Methodology

Run `scripts/bench.sh [label] [bytes]` against an online tether. It needs no root, but it does need `pgrep`, `stat`, `top`, `curl` and `python3` (for reading the status JSON). `label` is free text in the header (default `bench`); `bytes` is the download size (default 30000000). The script exits with an error if no running instance or no readable status file is found, or if the test download fails.

What the script measures, exactly:

- **CPU time** comes from `ps -o time=` for the tether process (not the crash guard), parsed from `[[dd-]hh:]mm:ss`.
- **Bytes carried** come from the tether's own counters: `traffic.rx_bytes + traffic.tx_bytes` in `/var/run/usb-android-tethering.json`, read with `python3`.
- **Both are sampled at the same instant**, right after the status file is rewritten. The script waits up to 7 s for the file's mtime to change, then reads CPU time, counters and wall-clock time together. The status file is rewritten at most every 5 s while online, and only when its contents change.
- **Load** is a download of `bytes` from `https://speed.cloudflare.com/__down?bytes=N`, with a 180 s timeout, discarded entirely if it fails.
- **Background cost** is a separate 10 s `top -l 2 -s 10 -pid PID` sample taken before the download: CPU percentage and idle wakeups.
- **Memory** is `ps -o rss=` at the end of the run.

The headline metric is **CPU per MB carried**: CPU seconds between the two samples divided by megabytes carried between them. Because background traffic (other apps, a VPN's keep-alives) is counted on both sides of the ratio, it doesn't skew the result.

Two numbers in the results below do not come from `scripts/bench.sh`; their methods are stated next to them (round-trip time from `ping`, and the BPF filter check from `netstat -B`). Everything else is a bench run.

Because the cost per byte depends on packet sizes (many small packets cost more than a few full-size ones), always compare builds on the same machine, phone and traffic mix.

## Results

**Environment:**
- MacBook, Apple Silicon, macOS 26.
- Samsung phone (`04e8:6863`), which accepts 15 frames / 23,700 B per transfer, alignment 16.
- A VPN (sing-box based) running over the tether and carrying continuous background traffic.

| Metric | Result |
|---|---|
| CPU per MB carried | **37–42 ms** (three runs; one run failed on a TLS error and was discarded) |
| CPU at 2.2 MB/s | 8.7–9.1 % of one core |
| CPU with only background VPN traffic | 3.6–5.5 % |
| Memory (RSS) | ~7 MB |
| Round-trip time to the phone | 2.9 ms average (`ping` to the phone's gateway address) |
| Frames macOS emits that aren't IPv4 or ARP | Dropped in the kernel, never copied to userspace (verified with `netstat -B`: 5 injected IPv6 frames rejected, ARP accepted) |

Sample output:

```
== run 1 (pid 3207)
background (10 s): cpu 5.5%  idle wakeups 64
download:          30000000 B at 2146988 B/s
carried:           35.1 MB in 15.0 s (2.3 MB/s)
cpu:               1.30 s = 8.7% of one core
cpu per MB:        37.1 ms
memory (RSS):      7.0 MB
```

### Optimisation history

| Version | Change | Measured effect |
|---|---|---|
| v1 | Synchronous transfers, one frame per transfer, 1 s polling | Baseline: 2.1 % CPU and ~78 wakeups/s with no traffic |
| v2 | Async receive ring (8) and transmit pool (8), RNDIS multi-packet aggregation, hotplug instead of polling | Wakeups with little traffic fell to about 1–4/s; CPU per MB fell by 20–40 % against v1 under the same conditions |
| v3 | Kernel BPF filter, self-pipe (no timer in the transmit thread), allocation-free packing, per-transfer counters, write-on-change status file | Wakeups with little traffic fell below 1/s |

> **Measurement note.** Figures quoted for v1–v3 during development divided the process's CPU time by the size of the test download only, ignoring background traffic, so they aren't comparable with the per-MB-carried numbers above. They are kept only as relative comparisons made under the same conditions. Use `scripts/bench.sh` for anything new.

## Tuning knobs (source constants)

| Constant | File | Default | Effect |
|---|---|---|---|
| `RX_QUEUE` | `tether.rs` | 8 | Receive transfers kept queued. Raise it if the phone ever stalls waiting for buffers at very high rates. |
| `TX_QUEUE` | `tether.rs` | 8 | Transmit transfers in flight. Raise it for heavy uploads over high-latency USB hubs. |
| `HOST_MAX_TRANSFER` | `rndis.rs` | 16 KiB | Largest receive transfer we accept, announced to the phone. It bounds the phone's download aggregation. |
| `BPF_BUF` | `feth.rs` | 256 KiB | Kernel buffer for frames from macOS. It bounds the batch size per read. |

## Ideas not yet pursued

- **Per-frame BPF writes on receive.** BPF accepts one frame per `write()`, so each received frame costs one system call. An `NDRV` socket has the same limit. A batched injection path would need a kernel component.
- **Larger download aggregation.** Android decides how many frames to aggregate per download transfer (`rndis_dl_max_pkt_per_xfer`, often 3 or less). Announcing a larger `HOST_MAX_TRANSFER` only helps phones configured for more.
