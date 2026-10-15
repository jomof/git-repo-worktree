#!/usr/bin/env python3
import json
import glob
import os
import re

R6 = "/usr/local/google/home/jomof/projects/git-repo-worktree/real-world-experience/round-6"

def parse_samples(samples_path):
    if not os.path.isfile(samples_path):
        return {}
    wchan_counts = {}
    dirty_vals = []
    with open(samples_path) as f:
        for line in f:
            if "Dirty:" in line:
                m = re.search(r'Dirty:\s*(\d+)\s*kB', line)
                if m:
                    dirty_vals.append(int(m.group(1)) / 1024.0)
            m_ps = re.match(r'^\s*(\d+)\s+(\S+)\s+(\S+)\s+(\S+)', line)
            if m_ps:
                count = int(m_ps.group(1))
                stat = m_ps.group(2)
                wchan = m_ps.group(3)
                comm = m_ps.group(4)
                key = f"{stat}:{wchan}:{comm}"
                wchan_counts[key] = wchan_counts.get(key, 0) + count
    mean_dirty = sum(dirty_vals)/len(dirty_vals) if dirty_vals else 0.0
    max_dirty = max(dirty_vals) if dirty_vals else 0.0
    sorted_wchans = sorted(wchan_counts.items(), key=lambda x: x[1], reverse=True)
    return {
        "mean_dirty": mean_dirty,
        "max_dirty": max_dirty,
        "top_wchans": sorted_wchans[:6]
    }

print("=== MAIN (AOSP) R6 ADD EVENTS ===")
with open(f"{R6}/data/main/history-r6.jsonl") as f:
    for line in f:
        d = json.loads(line)
        cmd = d.get("command")
        sess = d.get("session") or ""
        if cmd == "add":
            ph = d.get("phases", {})
            projs = d.get("projects", [])
            p_ms = [p.get("ms", 0) for p in projs]
            q_ms = [p.get("queue_ms", 0) for p in projs if "queue_ms" in p]
            p_ms.sort()
            q_ms.sort()
            p50 = p_ms[len(p_ms)//2] if p_ms else 0
            p90 = p_ms[int(len(p_ms)*0.9)] if p_ms else 0
            qp50 = q_ms[len(q_ms)//2] if q_ms else 0
            qp90 = q_ms[int(len(q_ms)*0.9)] if q_ms else 0
            hits = sum(1 for p in projs if p.get("strategy") == "pool")
            ech = d.get("estimate_cache_hits")
            # find samples
            prefix = sess.replace("R6-", "")
            samp_file = f"{R6}/data/main/{prefix}.samples"
            if not os.path.exists(samp_file):
                # try alternative name
                if "S4b" in sess:
                    samp_file = f"{R6}/data/main/A3-S4b.samples"
                elif "S4a" in sess:
                    samp_file = f"{R6}/data/main/A3-S4a.samples"
                elif "S4c" in sess:
                    samp_file = f"{R6}/data/main/A3-S4c-warm.samples"
            sdata = parse_samples(samp_file)
            wchans_str = "; ".join(f"{k}={v}" for k, v in sdata.get("top_wchans", []))
            print(f"ADD {sess} ms={d.get('ms')} hits={hits} ech={ech} snap={ph.get('snapshot')} (man={ph.get('snapshot-repo-manifest')},gd={ph.get('snapshot-git-dirs')},est={ph.get('snapshot-estimates')},pre={ph.get('snapshot-preflight')}) proj={ph.get('projects')} (idle={ph.get('projects-idle-slot')},jsync={ph.get('projects-journal-sync')},mwrite={ph.get('projects-meta-write')}) p_ms(p50={p50},p90={p90}) q_ms(p50={qp50},p90={qp90}) dirty_mean={sdata.get('mean_dirty',0):.1f} dirty_max={sdata.get('max_dirty',0):.1f} wchans=[{wchans_str}]")
        elif cmd == "remove":
            ph = d.get("phases", {})
            print(f"REMOVE {sess} ms={d.get('ms')} ret={d.get('returned_to_pool')} pm={d.get('pool_maintenance')} pruned={d.get('pruned_git_dirs')} ph={ph}")
        elif cmd == "pool maintain":
            ph = d.get("phases", {})
            print(f"MAINTAIN {sess} ms={d.get('ms')} skipped={d.get('skipped')} ph={ph} filled={len(d.get('filled') or [])}")

print("\n=== STUDIO-MAIN R6 EVENTS ===")
with open(f"{R6}/data/studio-main/history-r6.jsonl") as f:
    for line in f:
        d = json.loads(line)
        cmd = d.get("command")
        sess = d.get("session") or ""
        if cmd == "add":
            ph = d.get("phases", {})
            projs = d.get("projects", [])
            hits = sum(1 for p in projs if p.get("strategy") == "pool")
            ech = d.get("estimate_cache_hits")
            print(f"ADD {sess} ms={d.get('ms')} hits={hits} ech={ech} snap={ph.get('snapshot')} (est={ph.get('snapshot-estimates')}) proj={ph.get('projects')} idle={ph.get('projects-idle-slot')}")
        elif cmd == "remove":
            ph = d.get("phases", {})
            print(f"REMOVE {sess} ms={d.get('ms')} ret={d.get('returned_to_pool')} pm={d.get('pool_maintenance')} pool_ret={ph.get('pool-return')}")
        elif cmd == "pool maintain":
            ph = d.get("phases", {})
            print(f"MAINTAIN {sess} ms={d.get('ms')} skipped={d.get('skipped')} ph={ph} filled={len(d.get('filled') or [])}")
