#!/usr/bin/env python3
"""Turn docs/ROADMAP.md into GitHub labels, milestones, issues and a project board.

  roadmap_issues.py plan                  parse and print a summary (no network)
  roadmap_issues.py create OWNER/REPO     create everything with the gh CLI

`create` is resumable: it records what it made in tools/.roadmap_state.json and
skips anything already there.
"""
import json
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ROADMAP = ROOT / "docs" / "ROADMAP.md"
STATE = ROOT / "tools" / ".roadmap_state.json"

TRACK_NAMES = {
    "A": "storage",
    "B": "consensus and cluster",
    "C": "data model and client",
    "D": "simulation and infrastructure",
}
PHASE_4A = {"E4.1", "E4.2", "E4.3"}


def parse():
    phases, epics, tasks = {}, [], []
    phase = epic = None
    lines = ROADMAP.read_text().splitlines()
    for i, line in enumerate(lines):
        m = re.match(r"^## Phase (\d+)\. (.+?) \((.+)\)\s*$", line)
        if m:
            phase = m.group(1)
            exit_text = ""
            for nxt in lines[i + 1 : i + 4]:
                if nxt.startswith("Exit:"):
                    exit_text = nxt[len("Exit:") :].strip()
            phases[phase] = {"title": m.group(2), "note": m.group(3), "exit": exit_text}
            continue
        m = re.match(r"^### (E\d+\.\d+) (.+)$", line)
        if m:
            epic = {"id": m.group(1), "title": m.group(2), "phase": m.group(1)[1:].split(".")[0]}
            epics.append(epic)
            continue
        m = re.match(r"^\| (T\d+\.\d+\.\d+) \| (.+?) \| (.+?) \| (.+?) \|\s*$", line)
        if m and epic:
            tid, title, track, done = (s.strip() for s in m.groups())
            tasks.append(
                {
                    "id": tid,
                    "title": title,
                    "tracks": [t.strip() for t in track.split(",")],
                    "done": done,
                    "epic": epic["id"],
                    "phase": epic["phase"],
                }
            )
    return phases, epics, tasks


def milestone_name(phases, epic_id):
    phase = epic_id[1:].split(".")[0]
    title = phases[phase]["title"]
    if phase == "4":
        return "Phase 4a: Static multi-shard cluster" if epic_id in PHASE_4A else "Phase 4b: Tablet lifecycle"
    return f"Phase {phase}: {title}"


def task_kind(task):
    text = (task["title"] + " " + task["done"]).lower()
    if "model-check" in text or "annotate" in text or "specification" in text:
        return "kind:spec"
    if "benchmark" in text or "experiment" in text or "report" in text:
        return "kind:bench"
    if "checker" in text or "simulator" in text or "fuzz" in text:
        return "kind:test"
    return "kind:impl"


def task_labels(task):
    labels = [f"phase:{task['phase']}", task_kind(task)]
    for t in task["tracks"]:
        if t in TRACK_NAMES:
            labels.append(f"track:{t}")
    if re.search(r"\bG[123]\b", task["title"] + task["done"]):
        labels.append("gate")
    return labels


def task_body(task, epic):
    tracks = ", ".join(f"{t} ({TRACK_NAMES[t]})" if t in TRACK_NAMES else t for t in task["tracks"])
    return (
        f"**Epic:** {epic['id']} {epic['title']}\n"
        f"**Track:** {tracks}\n\n"
        f"## Done when\n\n{task['done']}\n\n"
        f"## References\n\n"
        f"- Roadmap: `docs/ROADMAP.md` ({task['id']})\n"
        f"- Design: `DESIGN.md` and the documents under `docs/`\n"
    )


def plan():
    phases, epics, tasks = parse()
    print(f"phases: {len(phases)}  epics: {len(epics)}  tasks: {len(tasks)}")
    by_ms = {}
    for e in epics:
        by_ms.setdefault(milestone_name(phases, e["id"]), [0, 0])[0] += 1
    for t in tasks:
        by_ms[milestone_name(phases, t["epic"])][1] += 1
    for name, (ne, nt) in by_ms.items():
        print(f"  {name}: {ne} epics, {nt} tasks")
    ids = [t["id"] for t in tasks]
    dupes = sorted({i for i in ids if ids.count(i) > 1})
    print("duplicate task ids:", dupes or "none")
    empty = [e["id"] for e in epics if not any(t["epic"] == e["id"] for t in tasks)]
    print("epics without tasks:", empty or "none")
    kinds = {}
    for t in tasks:
        kinds[task_kind(t)] = kinds.get(task_kind(t), 0) + 1
    print("kinds:", kinds)


def gh(*args, input_text=None):
    for attempt in range(5):
        r = subprocess.run(["gh", *args], input=input_text, capture_output=True, text=True)
        if r.returncode == 0:
            return r.stdout.strip()
        if "rate limit" in r.stderr.lower() or "secondary" in r.stderr.lower():
            time.sleep(60 * (attempt + 1))
            continue
        raise SystemExit(f"gh {' '.join(args[:4])} failed: {r.stderr.strip()}")
    raise SystemExit("gave up after repeated rate limiting")


def create(repo):
    phases, epics, tasks = parse()
    state = json.loads(STATE.read_text()) if STATE.exists() else {}
    state.setdefault("labels", []), state.setdefault("milestones", {}), state.setdefault("issues", {})

    def save():
        STATE.write_text(json.dumps(state, indent=1))

    labels = {f"phase:{p}": "1d76db" for p in phases}
    labels.update({f"track:{t}": "0e8a16" for t in TRACK_NAMES})
    labels.update({"kind:spec": "5319e7", "kind:impl": "c5def5", "kind:test": "fbca04", "kind:bench": "d93f0b"})
    labels.update({"epic": "3e4b9e", "gate": "b60205"})
    for name, color in labels.items():
        if name not in state["labels"]:
            gh("label", "create", name, "--repo", repo, "--color", color, "--force")
            state["labels"].append(name)
            save()

    for e in epics:
        name = milestone_name(phases, e["id"])
        if name not in state["milestones"]:
            desc = phases[e["phase"]]["exit"]
            out = gh("api", f"repos/{repo}/milestones", "-f", f"title={name}", "-f", f"description={desc}", "--jq", ".number")
            state["milestones"][name] = int(out)
            save()

    epic_by_id = {e["id"]: e for e in epics}

    def make_issue(key, title, body, label_list, milestone):
        if key in state["issues"]:
            return state["issues"][key]
        args = ["issue", "create", "--repo", repo, "--title", title, "--body-file", "-", "--milestone", milestone]
        for l in label_list:
            args += ["--label", l]
        url = gh(*args, input_text=body)
        number = int(url.rstrip("/").split("/")[-1])
        state["issues"][key] = number
        save()
        time.sleep(1.5)
        return number

    for t in tasks:
        epic = epic_by_id[t["epic"]]
        make_issue(t["id"], f"{t['id']} {t['title']}", task_body(t, epic), task_labels(t), milestone_name(phases, epic["id"]))

    for e in epics:
        children = [t for t in tasks if t["epic"] == e["id"]]
        checklist = "\n".join(f"- [ ] #{state['issues'][t['id']]} {t['id']}" for t in children)
        body = (
            f"Epic {e['id']} of phase {e['phase']} ({phases[e['phase']]['title']}).\n\n"
            f"**Phase exit criteria:** {phases[e['phase']]['exit'] or 'see roadmap'}\n\n"
            f"## Tasks\n\n{checklist}\n\n"
            f"Source: `docs/ROADMAP.md`.\n"
        )
        make_issue(e["id"], f"{e['id']} {e['title']}", body, ["epic", f"phase:{e['phase']}"], milestone_name(phases, e["id"]))

    print(f"done: {len(state['issues'])} issues in {repo}")


if __name__ == "__main__":
    if len(sys.argv) >= 2 and sys.argv[1] == "plan":
        plan()
    elif len(sys.argv) == 3 and sys.argv[1] == "create":
        create(sys.argv[2])
    else:
        sys.exit(__doc__)
