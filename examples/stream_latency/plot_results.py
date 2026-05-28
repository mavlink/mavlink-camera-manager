#!/usr/bin/env python3
"""
Compare stream-latency results across different camera bitrate configurations.

Usage:
    python plot_results.py results_16768.csv results_32768.csv results_51200.csv
    python plot_results.py results_*.csv
"""

import sys
import re
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import numpy as np
import pandas as pd
import matplotlib.pyplot as plt
from matplotlib.gridspec import GridSpec

CLIENTS = ["rtsp-0", "rtsp-1", "webrtc-0"]
CLIENT_LABELS = {
    "rtsp-0": "MCM RTSP",
    "rtsp-1": "Direct Camera",
    "webrtc-0": "MCM WebRTC",
}
PAIRS = [
    ("rtsp-0", "rtsp-1"),
    ("rtsp-0", "webrtc-0"),
    ("rtsp-1", "webrtc-0"),
]
PAIR_LABELS = {
    ("rtsp-0", "rtsp-1"): "MCM RTSP vs Direct Camera",
    ("rtsp-0", "webrtc-0"): "MCM RTSP vs MCM WebRTC",
    ("rtsp-1", "webrtc-0"): "Direct Camera vs MCM WebRTC",
}
PAIR_COLORS = {
    ("rtsp-0", "rtsp-1"): "#e74c3c",
    ("rtsp-0", "webrtc-0"): "#3498db",
    ("rtsp-1", "webrtc-0"): "#2ecc71",
}


def extract_bitrate_label(path: str) -> str:
    m = re.search(r"(\d+)", Path(path).stem)
    if m:
        kbps = int(m.group(1))
        return f"{kbps} kbps"
    return Path(path).stem


def compute_windowed_timeseries(arrivals_s: np.ndarray, bytes_arr: np.ndarray,
                                 window: float = 1.0, step: float = 0.5):
    """Compute FPS and bitrate in sliding windows over time."""
    if len(arrivals_s) < 2:
        return np.array([]), np.array([]), np.array([])
    t_start, t_end = arrivals_s[0], arrivals_s[-1]
    centers, fps_vals, bps_vals = [], [], []
    t = t_start + window / 2
    while t <= t_end - window / 2:
        mask = (arrivals_s >= t - window / 2) & (arrivals_s < t + window / 2)
        n = mask.sum()
        b = bytes_arr[mask].sum()
        fps_vals.append(n / window)
        bps_vals.append(b * 8 / window / 1e6)
        centers.append(t - t_start)
        t += step
    return np.array(centers), np.array(fps_vals), np.array(bps_vals)


def detect_drops_and_stutters(inter_arrival_ms: np.ndarray, nominal_fps: float):
    """Detect frame drops (gaps > 1.8x frame interval) and stutters (bursts)."""
    if len(inter_arrival_ms) < 2 or nominal_fps <= 0:
        return {"drops": 0, "drop_indices": [], "stutters": 0, "stutter_indices": []}
    expected_ms = 1000.0 / nominal_fps
    drop_threshold = expected_ms * 1.8
    stutter_threshold = expected_ms * 0.3

    drop_idx = np.where(inter_arrival_ms > drop_threshold)[0]
    stutter_idx = np.where(inter_arrival_ms < stutter_threshold)[0]

    missed_frames = 0
    for gap in inter_arrival_ms[drop_idx]:
        missed_frames += max(0, round(gap / expected_ms) - 1)

    return {
        "drops": len(drop_idx),
        "estimated_missed_frames": int(missed_frames),
        "drop_indices": drop_idx,
        "stutters": len(stutter_idx),
        "stutter_indices": stutter_idx,
    }


def load_and_compute(csv_path: str) -> dict:
    df = pd.read_csv(csv_path)

    result = {"path": csv_path, "label": extract_bitrate_label(csv_path), "df": df}

    for a, b in PAIRS:
        a_arr_col = f"{a}_arrival_us"
        b_arr_col = f"{b}_arrival_us"

        matched = df.dropna(subset=[a_arr_col, b_arr_col])
        if matched.empty:
            result[(a, b)] = {"deltas_ms": np.array([]), "n": 0, "n_total": len(df)}
            continue

        deltas_us = matched[b_arr_col].values - matched[a_arr_col].values
        result[(a, b)] = {
            "deltas_ms": deltas_us / 1000.0,
            "n": len(matched),
            "n_total": len(df),
        }

    for c in CLIENTS:
        arr_col = f"{c}_arrival_us"
        bytes_col = f"{c}_bytes"
        valid = df.dropna(subset=[arr_col])
        if len(valid) < 2:
            result[c] = {}
            continue

        arrivals_us = valid[arr_col].values
        bytes_arr = valid[bytes_col].values
        sort_idx = np.argsort(arrivals_us)
        arrivals_sorted = arrivals_us[sort_idx]
        bytes_sorted = bytes_arr[sort_idx]
        inter_arrival_ms = np.diff(arrivals_sorted) / 1000.0

        total_bytes = bytes_sorted.sum()
        duration_s = (arrivals_sorted[-1] - arrivals_sorted[0]) / 1e6
        n_frames = len(valid)
        fps = (n_frames - 1) / duration_s if duration_s > 0 else 0
        bitrate_mbps = (total_bytes * 8) / (duration_s * 1e6) if duration_s > 0 else 0
        avg_frame_kb = total_bytes / n_frames / 1024

        arrivals_s = arrivals_sorted / 1e6
        ts_time, ts_fps, ts_bitrate = compute_windowed_timeseries(
            arrivals_s, bytes_sorted, window=1.0, step=0.5
        )

        ds = detect_drops_and_stutters(inter_arrival_ms, fps)

        # Frame drops across clients: frames present in other clients but not this one
        other_clients = [x for x in CLIENTS if x != c]
        total_in_others = df.dropna(subset=[f"{o}_arrival_us" for o in other_clients]).shape[0]
        present_here = df.dropna(subset=[arr_col]).shape[0]
        cross_client_missing = max(0, total_in_others - present_here)

        result[c] = {
            "n_frames": n_frames,
            "fps": fps,
            "bitrate_mbps": bitrate_mbps,
            "avg_frame_kb": avg_frame_kb,
            "inter_arrival_ms": inter_arrival_ms,
            "jitter_stddev_ms": np.std(inter_arrival_ms),
            "ts_time": ts_time,
            "ts_fps": ts_fps,
            "ts_bitrate": ts_bitrate,
            "drops": ds["drops"],
            "estimated_missed_frames": ds["estimated_missed_frames"],
            "stutters": ds["stutters"],
            "drop_indices": ds["drop_indices"],
            "stutter_indices": ds["stutter_indices"],
            "arrivals_sorted_s": arrivals_s - arrivals_s[0],
            "cross_client_missing": cross_client_missing,
        }

    return result


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(1)

    csv_paths = sorted(sys.argv[1:])
    datasets = [load_and_compute(p) for p in csv_paths]

    bitrate_labels = [d["label"] for d in datasets]
    n_datasets = len(datasets)
    bitrate_cmap = plt.cm.viridis(np.linspace(0.15, 0.85, n_datasets))

    plt.style.use("seaborn-v0_8-whitegrid")
    plt.rcParams.update({"font.size": 10, "figure.dpi": 120})

    # ── Figure 1: Latency distributions (histograms + box plots) ──────────

    fig1 = plt.figure(figsize=(16, 10))
    fig1.suptitle("Pairwise Latency Distributions by Camera Bitrate", fontsize=14, y=0.98)
    gs = GridSpec(2, 3, figure=fig1, hspace=0.35, wspace=0.3, top=0.92)

    for pi, (a, b) in enumerate(PAIRS):
        # Histogram row
        ax_hist = fig1.add_subplot(gs[0, pi])
        for di, d in enumerate(datasets):
            data = d[(a, b)]["deltas_ms"]
            if len(data) == 0:
                continue
            ax_hist.hist(
                data,
                bins=60,
                alpha=0.5,
                color=bitrate_cmap[di],
                label=d["label"],
                density=True,
            )
        ax_hist.set_xlabel("Latency delta (ms)")
        ax_hist.set_ylabel("Density")
        ax_hist.set_title(PAIR_LABELS[(a, b)], fontsize=11)
        ax_hist.legend(fontsize=8)

        # Box plot row
        ax_box = fig1.add_subplot(gs[1, pi])
        bp_data = []
        bp_labels = []
        for di, d in enumerate(datasets):
            data = d[(a, b)]["deltas_ms"]
            if len(data) > 0:
                bp_data.append(data)
                bp_labels.append(d["label"])

        if bp_data:
            bp = ax_box.boxplot(
                bp_data,
                tick_labels=bp_labels,
                patch_artist=True,
                showfliers=True,
                flierprops=dict(marker=".", markersize=2, alpha=0.4),
                medianprops=dict(color="black", linewidth=1.5),
            )
            for patch, color in zip(bp["boxes"], bitrate_cmap[: len(bp_data)]):
                patch.set_facecolor(color)
                patch.set_alpha(0.6)
        ax_box.set_ylabel("Latency delta (ms)")
        ax_box.set_title(PAIR_LABELS[(a, b)], fontsize=11)

    fig1.savefig("latency_distributions.png", bbox_inches="tight")
    print("Saved latency_distributions.png")

    # ── Figure 2: Per-client stats comparison ─────────────────────────────

    fig2, axes2 = plt.subplots(2, 2, figsize=(14, 9))
    fig2.suptitle("Per-Client Stream Statistics by Camera Bitrate", fontsize=14)

    x = np.arange(n_datasets)
    width = 0.22
    client_colors = ["#e74c3c", "#3498db", "#2ecc71"]

    # Frame rate
    ax = axes2[0, 0]
    for ci, c in enumerate(CLIENTS):
        vals = [d[c].get("fps", 0) for d in datasets]
        ax.bar(x + ci * width, vals, width, label=CLIENT_LABELS[c], color=client_colors[ci], alpha=0.8)
    ax.set_ylabel("FPS")
    ax.set_title("Frame Rate")
    ax.set_xticks(x + width)
    ax.set_xticklabels(bitrate_labels, fontsize=9)
    ax.legend(fontsize=8)

    # Measured bitrate
    ax = axes2[0, 1]
    for ci, c in enumerate(CLIENTS):
        vals = [d[c].get("bitrate_mbps", 0) for d in datasets]
        ax.bar(x + ci * width, vals, width, label=CLIENT_LABELS[c], color=client_colors[ci], alpha=0.8)
    ax.set_ylabel("Mbps")
    ax.set_title("Measured Bitrate")
    ax.set_xticks(x + width)
    ax.set_xticklabels(bitrate_labels, fontsize=9)
    ax.legend(fontsize=8)

    # Average frame size
    ax = axes2[1, 0]
    for ci, c in enumerate(CLIENTS):
        vals = [d[c].get("avg_frame_kb", 0) for d in datasets]
        ax.bar(x + ci * width, vals, width, label=CLIENT_LABELS[c], color=client_colors[ci], alpha=0.8)
    ax.set_ylabel("KB")
    ax.set_title("Average Frame Size")
    ax.set_xticks(x + width)
    ax.set_xticklabels(bitrate_labels, fontsize=9)
    ax.legend(fontsize=8)

    # Jitter (stddev of inter-arrival)
    ax = axes2[1, 1]
    for ci, c in enumerate(CLIENTS):
        vals = [d[c].get("jitter_stddev_ms", 0) for d in datasets]
        ax.bar(x + ci * width, vals, width, label=CLIENT_LABELS[c], color=client_colors[ci], alpha=0.8)
    ax.set_ylabel("ms")
    ax.set_title("Jitter (stddev of inter-arrival)")
    ax.set_xticks(x + width)
    ax.set_xticklabels(bitrate_labels, fontsize=9)
    ax.legend(fontsize=8)

    fig2.tight_layout()
    fig2.savefig("client_stats.png", bbox_inches="tight")
    print("Saved client_stats.png")

    # ── Figure 3: Inter-arrival time distributions ────────────────────────

    fig3, axes3 = plt.subplots(1, 3, figsize=(16, 5))
    fig3.suptitle("Inter-Arrival Time Distributions by Camera Bitrate", fontsize=14, y=1.02)

    for ci, c in enumerate(CLIENTS):
        ax = axes3[ci]
        for di, d in enumerate(datasets):
            ia = d[c].get("inter_arrival_ms", np.array([]))
            if len(ia) == 0:
                continue
            ax.hist(
                ia,
                bins=80,
                alpha=0.5,
                color=bitrate_cmap[di],
                label=d["label"],
                density=True,
                range=(0, min(200, np.percentile(ia, 99.5))),
            )
        ax.set_xlabel("Inter-arrival time (ms)")
        ax.set_ylabel("Density")
        ax.set_title(CLIENT_LABELS[c], fontsize=11)
        ax.legend(fontsize=8)

    fig3.tight_layout()
    fig3.savefig("inter_arrival.png", bbox_inches="tight")
    print("Saved inter_arrival.png")

    # ── Figure 4: Latency summary bar chart ───────────────────────────────

    fig4, ax4 = plt.figure(figsize=(12, 6)), None
    ax4 = fig4.add_subplot(111)
    fig4.suptitle("Median Latency (p50) by Pair and Camera Bitrate", fontsize=14)

    x = np.arange(len(PAIRS))
    width = 0.8 / n_datasets

    for di, d in enumerate(datasets):
        medians = []
        for pair in PAIRS:
            data = d[pair]["deltas_ms"]
            medians.append(np.median(data) if len(data) > 0 else 0)
        bars = ax4.bar(
            x + di * width,
            medians,
            width,
            label=d["label"],
            color=bitrate_cmap[di],
            alpha=0.8,
        )
        for bar, val in zip(bars, medians):
            ax4.text(
                bar.get_x() + bar.get_width() / 2,
                bar.get_height() + (0.5 if val >= 0 else -1.5),
                f"{val:.1f}",
                ha="center",
                va="bottom" if val >= 0 else "top",
                fontsize=8,
            )

    ax4.set_ylabel("Latency delta (ms)")
    ax4.set_xticks(x + width * (n_datasets - 1) / 2)
    ax4.set_xticklabels([PAIR_LABELS[p] for p in PAIRS], fontsize=9)
    ax4.legend(fontsize=9)
    ax4.axhline(0, color="gray", linewidth=0.5, linestyle="--")

    fig4.tight_layout()
    fig4.savefig("latency_summary.png", bbox_inches="tight")
    print("Saved latency_summary.png")

    # ── Figure 5: FPS over time ──────────────────────────────────────────

    fig5, axes5 = plt.subplots(1, n_datasets, figsize=(6 * n_datasets, 5), squeeze=False)
    fig5.suptitle("FPS Over Time (1s sliding window)", fontsize=14, y=1.02)

    for di, d in enumerate(datasets):
        ax = axes5[0, di]
        for ci, c in enumerate(CLIENTS):
            s = d[c]
            if not s or len(s.get("ts_time", [])) == 0:
                continue
            ax.plot(s["ts_time"], s["ts_fps"], label=CLIENT_LABELS[c],
                    color=client_colors[ci], alpha=0.85, linewidth=1)
        ax.set_xlabel("Time (s)")
        ax.set_ylabel("FPS")
        ax.set_title(d["label"], fontsize=11)
        ax.legend(fontsize=8)
        ax.set_ylim(bottom=0)

    fig5.tight_layout()
    fig5.savefig("fps_over_time.png", bbox_inches="tight")
    print("Saved fps_over_time.png")

    # ── Figure 6: Bitrate over time ──────────────────────────────────────

    fig6, axes6 = plt.subplots(1, n_datasets, figsize=(6 * n_datasets, 5), squeeze=False)
    fig6.suptitle("Bitrate Over Time (1s sliding window)", fontsize=14, y=1.02)

    for di, d in enumerate(datasets):
        ax = axes6[0, di]
        for ci, c in enumerate(CLIENTS):
            s = d[c]
            if not s or len(s.get("ts_time", [])) == 0:
                continue
            ax.plot(s["ts_time"], s["ts_bitrate"], label=CLIENT_LABELS[c],
                    color=client_colors[ci], alpha=0.85, linewidth=1)
        ax.set_xlabel("Time (s)")
        ax.set_ylabel("Bitrate (Mbps)")
        ax.set_title(d["label"], fontsize=11)
        ax.legend(fontsize=8)
        ax.set_ylim(bottom=0)

    fig6.tight_layout()
    fig6.savefig("bitrate_over_time.png", bbox_inches="tight")
    print("Saved bitrate_over_time.png")

    # ── Figure 7: Drops & stutters summary ───────────────────────────────

    fig7, axes7 = plt.subplots(1, 3, figsize=(16, 5))
    fig7.suptitle("Frame Drops & Stutters by Camera Bitrate", fontsize=14, y=1.02)

    x_ds = np.arange(n_datasets)
    w_ds = 0.22

    # Drops (gap events)
    ax = axes7[0]
    for ci, c in enumerate(CLIENTS):
        vals = [d[c].get("drops", 0) for d in datasets]
        ax.bar(x_ds + ci * w_ds, vals, w_ds, label=CLIENT_LABELS[c],
               color=client_colors[ci], alpha=0.8)
    ax.set_ylabel("Count")
    ax.set_title("Gap Events (>1.8x expected interval)")
    ax.set_xticks(x_ds + w_ds)
    ax.set_xticklabels(bitrate_labels, fontsize=9)
    ax.legend(fontsize=8)

    # Estimated missed frames
    ax = axes7[1]
    for ci, c in enumerate(CLIENTS):
        vals = [d[c].get("estimated_missed_frames", 0) for d in datasets]
        ax.bar(x_ds + ci * w_ds, vals, w_ds, label=CLIENT_LABELS[c],
               color=client_colors[ci], alpha=0.8)
    ax.set_ylabel("Count")
    ax.set_title("Estimated Missed Frames")
    ax.set_xticks(x_ds + w_ds)
    ax.set_xticklabels(bitrate_labels, fontsize=9)
    ax.legend(fontsize=8)

    # Stutters (burst events)
    ax = axes7[2]
    for ci, c in enumerate(CLIENTS):
        vals = [d[c].get("stutters", 0) for d in datasets]
        ax.bar(x_ds + ci * w_ds, vals, w_ds, label=CLIENT_LABELS[c],
               color=client_colors[ci], alpha=0.8)
    ax.set_ylabel("Count")
    ax.set_title("Stutter Events (<0.3x expected interval)")
    ax.set_xticks(x_ds + w_ds)
    ax.set_xticklabels(bitrate_labels, fontsize=9)
    ax.legend(fontsize=8)

    fig7.tight_layout()
    fig7.savefig("drops_and_stutters.png", bbox_inches="tight")
    print("Saved drops_and_stutters.png")

    # ── Print summary table ───────────────────────────────────────────────

    print("\n" + "=" * 90)
    print("SUMMARY TABLE")
    print("=" * 90)

    for d in datasets:
        print(f"\n--- {d['label']} ({Path(d['path']).name}) ---")
        for c in CLIENTS:
            s = d[c]
            if not s:
                continue
            print(
                f"  {CLIENT_LABELS[c]:20s}: {s['n_frames']:4d} frames, "
                f"{s['fps']:.1f} fps, {s['bitrate_mbps']:.1f} Mbps, "
                f"{s['avg_frame_kb']:.0f} KB/frame, "
                f"jitter={s['jitter_stddev_ms']:.1f}ms, "
                f"drops={s['drops']} (~{s['estimated_missed_frames']} missed), "
                f"stutters={s['stutters']}, "
                f"cross-missing={s['cross_client_missing']}"
            )
        for pair in PAIRS:
            info = d[pair]
            data = info["deltas_ms"]
            if len(data) == 0:
                print(f"  {PAIR_LABELS[pair]:42s}: no matches")
                continue
            n, total = info["n"], info["n_total"]
            p50, p95, p99 = np.percentile(data, [50, 95, 99])
            print(
                f"  {PAIR_LABELS[pair]:42s}: {n}/{total} matched  "
                f"avg={np.mean(data):+.1f}ms  p50={p50:+.1f}ms  "
                f"p95={p95:+.1f}ms  p99={p99:+.1f}ms"
            )

    if "--show" in sys.argv:
        plt.show()


if __name__ == "__main__":
    main()
