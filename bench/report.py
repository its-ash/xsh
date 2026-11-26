"""Summarise hyperfine JSON files from bench/run.sh into a comparison table."""

import json
import math
import pathlib
import platform
import subprocess
import sys

SHELLS = ("xsh", "bash", "zsh")


def ms(s):
    return f"{s * 1000:.1f}" if s < 10 else f"{s:.2f}s"


def fmt(r):
    return f"{ms(r['mean'])} ± {ms(r['stddev'] or 0)}"


def ratio(x, other):
    if not x or not other:
        return "-"
    f = other / x
    return f"{f:.2f}× faster" if f >= 1 else f"{1 / f:.2f}× slower"


def parse_results(doc):
    out = {}
    for r in doc.get("results", []):
        key = r.get("name") or r.get("command")
        summary = r.get("summary")
        if summary and "time_wall_clock" in summary:
            t = summary["time_wall_clock"]
            rss = summary.get("memory_peak_resident", {}).get("mean")
            out[key] = {"mean": t["mean"], "stddev": t.get("stddev", 0.0), "rss": rss}
        elif "mean" in r:
            out[key] = {"mean": r["mean"], "stddev": r.get("stddev", 0.0), "rss": None}
    return out


def main():
    out = pathlib.Path(sys.argv[1])
    version = sys.argv[2] if len(sys.argv) > 2 else "xsh"
    index = out / "index.tsv"
    if not index.exists():
        sys.exit("bench: no results")
    rows, logs, peak_rss = [], {s: [] for s in SHELLS}, {s: 0.0 for s in SHELLS}
    for line in index.read_text().splitlines():
        wid, label, mismatch = (line.split("\t") + ["", ""])[:3]
        doc = json.loads((out / f"{wid}.json").read_text())
        res = parse_results(doc)
        for s in SHELLS:
            if s in res and res[s]["mean"] > 0:
                logs[s].append(math.log(res[s]["mean"]))
                if res[s]["rss"]:
                    peak_rss[s] = max(peak_rss[s], res[s]["rss"])
        rows.append((label, res, mismatch.strip()))

    head = ["Workload (mean ± σ, ms)", "xsh", "bash 3.2", "zsh", "xsh vs bash", "xsh vs zsh"]
    table = [head]
    for label, res, mismatch in rows:
        x = res.get("xsh", {}).get("mean")
        cells = [label + (" ⚠" if mismatch else "")]
        cells += [fmt(res[s]) if s in res else "-" for s in SHELLS]
        cells += [ratio(x, res.get("bash", {}).get("mean")), ratio(x, res.get("zsh", {}).get("mean"))]
        table.append(cells)
    geo = {s: math.exp(sum(v) / len(v)) for s, v in logs.items() if v}
    table.append(
        ["Geometric mean", *[ms(geo[s]) if s in geo else "-" for s in SHELLS],
         ratio(geo.get("xsh"), geo.get("bash")), ratio(geo.get("xsh"), geo.get("zsh"))]
    )
    if any(peak_rss.values()):
        mb = lambda b: f"{b / (1024 * 1024):.1f} MiB" if b else "-"
        table.append(["Max peak RSS", *[mb(peak_rss[s]) for s in SHELLS], "-", "-"])

    widths = [max(len(r[i]) for r in table) for i in range(len(head))]
    line = lambda r: "| " + " | ".join(c.ljust(w) for c, w in zip(r, widths)) + " |"
    md = [line(table[0]), "| " + " | ".join("-" * w for w in widths) + " |"] + [line(r) for r in table[1:]]

    cpu = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True).stdout.strip()
    meta = f"{version} · {cpu} · macOS {platform.mac_ver()[0]}"
    notes = "\n⚠ = the shell's output differs from xsh for that workload (see target/bench/mismatch.txt)." if any(m for _, _, m in rows) else ""
    text = f"# xsh benchmark\n\n{meta}\n\n" + "\n".join(md) + "\n" + notes + "\n"
    (out / "results.md").write_text(text)
    print()
    print("\n".join(md))
    if notes:
        print(notes.strip())
    print(f"\nSaved {out / 'results.md'}")


if __name__ == "__main__":
    main()
