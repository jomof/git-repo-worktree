#!/usr/bin/env python3
import glob
import json
import os
import re
import subprocess

R5 = "/usr/local/google/home/jomof/projects/git-repo-worktree/real-world-experience/round-5"
STUDIO = "/usr/local/google/home/jomof/projects/studio-main"
MAIN = "/usr/local/google/home/jomof/projects/main"


def pct(vals, p):
  if not vals:
    return 0
  s = sorted(vals)
  idx = int((len(s) - 1) * p)
  return s[idx]


def load_jsonl(path):
  out = []
  with open(path, "r", encoding="utf-8") as f:
    for line in f:
      line = line.strip()
      if line:
        out.append(json.loads(line))
  return out


def parse_time_file(path):
  info = {}
  if not os.path.exists(path):
    return info
  with open(path, "r", encoding="utf-8") as f:
    for line in f:
      if "User time (seconds):" in line:
        info["user_s"] = float(line.split(":")[-1].strip())
      elif "System time (seconds):" in line:
        info["sys_s"] = float(line.split(":")[-1].strip())
      elif "Percent of CPU this job got:" in line:
        info["cpu_pct"] = line.split(":")[-1].strip()
      elif "Elapsed (wall clock) time" in line:
        info["wall"] = line.split("): ")[-1].strip()
      elif "Maximum resident set size" in line:
        info["rss_kb"] = int(line.split(":")[-1].strip())
  return info


def parse_samples(path):
  # Sum thread counts by (stat, wchan, comm) and also track max Dirty kB and max load1
  hist = {}
  dirty_vals = []
  load_vals = []
  ticks = 0
  if not os.path.exists(path):
    return {"ticks": 0, "top": [], "max_dirty_mb": 0, "mean_dirty_mb": 0, "mean_load1": 0}
  with open(path, "r", encoding="utf-8") as f:
    for line in f:
      line_s = line.strip()
      if line_s.startswith("t="):
        ticks += 1
      elif re.match(r"^\d+\.\d+\s+\d+\.\d+\s+\d+\.\d+", line_s):
        load_vals.append(float(line_s.split()[0]))
      elif line_s.startswith("Dirty:"):
        m = re.search(r"Dirty:\s+(\d+)\s+kB", line_s)
        if m:
          dirty_vals.append(int(m.group(1)) / 1024.0)
      else:
        # e.g. "     12 D    balance_dirty_pages              git"
        m = re.match(r"^(\d+)\s+(\S+)\s+(\S+)\s+(.+)$", line_s)
        if m:
          cnt = int(m.group(1))
          st = m.group(2)
          wchan = m.group(3)
          comm = m.group(4).strip()
          if comm == "ps" or comm == "time":
            continue
          key = (st, wchan, comm)
          hist[key] = hist.get(key, 0) + cnt
  top = sorted(hist.items(), key=lambda kv: -kv[1])[:12]
  return {
      "ticks": ticks,
      "top": top,
      "max_dirty_mb": round(max(dirty_vals), 1) if dirty_vals else 0,
      "mean_dirty_mb": round(sum(dirty_vals) / len(dirty_vals), 1) if dirty_vals else 0,
      "mean_load1": round(sum(load_vals) / len(load_vals), 1) if load_vals else 0,
  }


def main():
  studio_hist = load_jsonl(f"{STUDIO}/.repo/repo-worktrees/history.jsonl")
  main_hist = load_jsonl(f"{MAIN}/.repo/repo-worktrees/history.jsonl")

  studio_r5 = [e for e in studio_hist if str(e.get("session", "")).startswith("R5-") or e.get("commit") == "a9a71ee28036"]
  main_r5 = [e for e in main_hist if str(e.get("session", "")).startswith("R5-") or e.get("commit") == "a9a71ee28036"]

  with open(f"{R5}/data/studio-main/history-r5.jsonl", "w", encoding="utf-8") as f:
    for e in studio_r5:
      f.write(json.dumps(e) + "\n")
  with open(f"{R5}/data/main/history-r5.jsonl", "w", encoding="utf-8") as f:
    for e in main_r5:
      f.write(json.dumps(e) + "\n")

  print("=== STUDIO-MAIN R5 EVENTS ===")
  for e in studio_r5:
    cmd = e.get("command")
    sess = e.get("session")
    ms = e.get("ms")
    phases = e.get("phases", {})
    if cmd == "add":
      projs = e.get("projects", [])
      pool_hits = sum(1 for p in projs if p.get("strategy") == "pool")
      q_ms = [p.get("queue_ms", 0) for p in projs]
      p_ms = [p.get("ms", 0) for p in projs]
      slowest = sorted(projs, key=lambda p: -p.get("ms", 0))[:3]
      slow_str = ", ".join(f"{p['path']}:{p['ms']}ms(q={p.get('queue_ms')}ms,{p.get('strategy')})" for p in slowest)
      print(f"ADD {sess} tree={e.get('tree')} ms={ms} pool_hits={pool_hits} phases={phases} q_p50={pct(q_ms,0.5)} q_p90={pct(q_ms,0.9)} q_max={max(q_ms) if q_ms else 0} slowest=[{slow_str}]")
    elif cmd == "pool maintain":
      filled = e.get("filled", [])
      trimmed = e.get("trimmed", [])
      reaped = e.get("reaped", [])
      print(f"MAINTAIN {sess} ms={ms} skipped={e.get('skipped')} phases={phases} filled={len(filled)} trimmed={trimmed} reaped={reaped} deferred={e.get('deferred')}")
    elif cmd == "remove":
      print(f"REMOVE {sess} tree={e.get('tree')} ms={ms} ret={e.get('returned_to_pool')} pruned_git_dirs={e.get('pruned_git_dirs')} phases={phases}")
    else:
      print(f"OTHER {cmd} {sess} ms={ms}")

  print("\n=== MAIN (AOSP) R5 ADD EVENTS ===")
  for e in main_r5:
    cmd = e.get("command")
    sess = e.get("session")
    ms = e.get("ms")
    phases = e.get("phases", {})
    if cmd == "add":
      projs = e.get("projects", [])
      pool_hits = sum(1 for p in projs if p.get("strategy") == "pool")
      q_ms = [p.get("queue_ms", 0) for p in projs]
      p_ms = [p.get("ms", 0) for p in projs]
      label = sess.replace("R5-", "") if sess else ""
      tinfo = parse_time_file(f"{R5}/data/main/{label}.time")
      sinfo = parse_samples(f"{R5}/data/main/{label}.samples")
      if not sinfo["ticks"] and label == "S4-add2-during-maintain":
        tinfo = parse_time_file(f"{R5}/data/main/S4-add2.time")
        sinfo = parse_samples(f"{R5}/data/main/S4-add2.samples")
      top_wchan = "; ".join(f"{st}:{w}:{c}={cnt}" for (st, w, c), cnt in sinfo["top"][:6])
      print(
          f"ADD {sess} ms={ms} pool_hits={pool_hits} cpu={tinfo.get('cpu_pct')} "
          f"snap={phases.get('snapshot')} (manifest={phases.get('snapshot-repo-manifest')},gitdirs={phases.get('snapshot-git-dirs')},est={phases.get('snapshot-estimates')},pre={phases.get('snapshot-preflight')}) "
          f"proj={phases.get('projects')} (jsync={phases.get('projects-journal-sync')},mwrite={phases.get('projects-meta-write')}) bridge={phases.get('repo-bridge')} "
          f"p_ms(p50={pct(p_ms,0.5)},p90={pct(p_ms,0.9)},max={max(p_ms)}) "
          f"q_ms(p50={pct(q_ms,0.5)},p90={pct(q_ms,0.9)},max={max(q_ms)}) "
          f"dirty_mb(mean={sinfo['mean_dirty_mb']},max={sinfo['max_dirty_mb']}) load1_mean={sinfo['mean_load1']} "
          f"sys_start={e.get('system_start',{}).get('loadavg')} "
          f"wchan=[{top_wchan}]"
      )
    elif cmd == "pool maintain":
      filled = e.get("filled", [])
      print(f"MAINTAIN {sess} ms={ms} skipped={e.get('skipped')} phases={phases} filled={len(filled)} deferred={e.get('deferred')}")
    elif cmd == "remove":
      print(f"REMOVE {sess} tree={e.get('tree')} ms={ms} ret={e.get('returned_to_pool')} pruned_git_dirs={e.get('pruned_git_dirs')} phases={phases}")


if __name__ == "__main__":
  main()
