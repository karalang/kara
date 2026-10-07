#!/usr/bin/env python3
"""Python mirror of source.orig.kara: a small entity-component system."""

ARENA_MAX = 16
SIGHT = 14
TICKS = 10

ROSTER = [
    "0 red knight 0 0",
    "0 red archer 1 4",
    "0 red tower 2 2",
    "0 blue raider 12 1",
    "0 blue brute 10 6",
    "0 blue scout 8 3",
    "0 blue beacon 14 8",
    "3 red squire 0 5",
    "4 blue raider 15 2",
    "6 green knight 1 1",
    "7 red dragon 3 3",
    "x red knight 1 1",
    "8 blue scout",
]


class SetupError(Exception):
    def __init__(self, kind, text):
        self.kind = kind
        self.text = text

    def describe(self):
        if self.kind == "BadLine":
            return f"malformed roster line '{self.text}'"
        if self.kind == "UnknownTeam":
            return f"unknown team '{self.text}'"
        if self.kind == "UnknownKind":
            return f"unknown unit kind '{self.text}'"
        return f"not a number: '{self.text}'"


# kind -> (hp, speed, weapon(damage, range, cooldown) or None)
TEMPLATES = {
    "knight": (40, 2, (6, 1, 0)),
    "archer": (20, 1, (4, 5, 1)),
    "tower": (60, 0, (3, 6, 0)),
    "squire": (25, 2, (4, 1, 0)),
    "raider": (25, 3, (5, 1, 0)),
    "brute": (50, 1, (8, 1, 1)),
    "scout": (12, 2, (2, 3, 0)),
    "beacon": (15, 0, None),
}


def parse_number(text):
    try:
        if text.strip() != text or text == "":
            raise ValueError
        return int(text)
    except ValueError:
        raise SetupError("BadNumber", text)


def parse_order(line):
    parts = line.split(" ")
    if len(parts) != 5:
        raise SetupError("BadLine", line)
    tick = parse_number(parts[0])
    if parts[1] == "red":
        team = "red"
    elif parts[1] == "blue":
        team = "blue"
    else:
        raise SetupError("UnknownTeam", parts[1])
    kind = parts[2]
    if kind not in TEMPLATES:
        raise SetupError("UnknownKind", kind)
    x = parse_number(parts[3])
    y = parse_number(parts[4])
    return (tick, team, kind, (x, y))


def sign(v):
    return 1 if v > 0 else (-1 if v < 0 else 0)


def clamp(v, lo, hi):
    return lo if v < lo else (hi if v > hi else v)


def step_toward(delta, speed):
    mag = abs(delta)
    step = speed if speed < mag else mag
    return sign(delta) * step


def label(ref):
    return f"#{ref[0]}.{ref[1]}"


class World:
    def __init__(self):
        self.slots = []  # dict(generation, alive, name, team)
        self.free = []
        self.positions = []
        self.motions = []  # dict(vel, speed) or None
        self.healths = []  # [current, max] or None
        self.weapons = []  # dict or None
        self.targets = []
        self.log = []

    def is_alive(self, e):
        i, g = e
        return 0 <= i < len(self.slots) and self.slots[i]["alive"] and self.slots[i]["generation"] == g

    def name_of(self, e):
        return self.slots[e[0]]["name"]

    def spawn(self, kind, team, pos):
        hp, speed, weapon = TEMPLATES[kind]
        if self.free:
            index = self.free.pop()
            generation = self.slots[index]["generation"] + 1
            self.slots[index] = dict(generation=generation, alive=True, name=kind, team=team)
            self.positions[index] = pos
            self.motions[index] = None
            self.healths[index] = None
            self.weapons[index] = None
            self.targets[index] = None
        else:
            index = len(self.slots)
            generation = 0
            self.slots.append(dict(generation=0, alive=True, name=kind, team=team))
            self.positions.append(pos)
            self.motions.append(None)
            self.healths.append(None)
            self.weapons.append(None)
            self.targets.append(None)
        e = (index, generation)
        self.healths[index] = [hp, hp]
        if speed > 0:
            self.motions[index] = dict(vel=(0, 0), speed=speed)
        if weapon is not None:
            d, r, c = weapon
            self.weapons[index] = dict(damage=d, range=r, cooldown=c, ready_in=0)
        return e

    def despawn(self, e):
        i = e[0]
        self.slots[i]["alive"] = False
        self.positions[i] = None
        self.motions[i] = None
        self.healths[i] = None
        self.weapons[i] = None
        self.targets[i] = None
        self.free.append(i)

    def living(self):
        return [(i, s["generation"]) for i, s in enumerate(self.slots) if s["alive"]]

    def nearest_enemy(self, e):
        me = self.positions[e[0]]
        team = self.slots[e[0]]["team"]
        best = None
        best_dist = 0
        for other in self.living():
            if self.slots[other[0]]["team"] == team or self.healths[other[0]] is None:
                continue
            p = self.positions[other[0]]
            d = abs(me[0] - p[0]) + abs(me[1] - p[1])
            if d > SIGHT:
                continue
            if best is None or d < best_dist:
                best = other
                best_dist = d
        return best

    def run_targeting(self):
        for e in self.living():
            i = e[0]
            if self.weapons[i] is None:
                continue
            cur = self.targets[i]
            if cur is not None:
                if self.is_alive(cur):
                    continue
                if self.slots[cur[0]]["alive"]:
                    self.log.append(f"{self.name_of(e)} {label(e)} dropped stale target {label(cur)} (slot reused)")
                else:
                    self.log.append(f"{self.name_of(e)} {label(e)} lost target {label(cur)} (destroyed)")
                self.targets[i] = None
            found = self.nearest_enemy(e)
            if found is not None:
                self.targets[i] = found
                self.log.append(f"{self.name_of(e)} {label(e)} targets {self.name_of(found)} {label(found)}")

    def run_movement(self):
        for e in self.living():
            i = e[0]
            m = self.motions[i]
            if m is None:
                continue
            pos = self.positions[i]
            vel = (0, 0)
            t = self.targets[i]
            if t is not None and self.is_alive(t):
                tp = self.positions[t[0]]
                rng = self.weapons[i]["range"]
                dx, dy = tp[0] - pos[0], tp[1] - pos[1]
                if abs(dx) + abs(dy) > rng:
                    if abs(dx) >= abs(dy):
                        vel = (step_toward(dx, m["speed"]), 0)
                    else:
                        vel = (0, step_toward(dy, m["speed"]))
            m["vel"] = vel
            self.positions[i] = (clamp(pos[0] + vel[0], 0, ARENA_MAX), clamp(pos[1] + vel[1], 0, ARENA_MAX))

    def run_combat(self):
        hits = []
        for e in self.living():
            i = e[0]
            w = self.weapons[i]
            if w is None:
                continue
            if w["ready_in"] > 0:
                w["ready_in"] -= 1
                continue
            t = self.targets[i]
            if t is None or not self.is_alive(t):
                continue
            a, b = self.positions[i], self.positions[t[0]]
            if abs(a[0] - b[0]) + abs(a[1] - b[1]) <= w["range"]:
                hits.append((e, t, w["damage"]))
                w["ready_in"] = w["cooldown"]
        for attacker, target, amount in hits:
            if not self.is_alive(attacker):
                self.log.append(f"{label(attacker)} fell before it could strike {label(target)}")
                continue
            if not self.is_alive(target):
                self.log.append(f"{self.name_of(attacker)} {label(attacker)} shot at {label(target)} but it was already gone")
                continue
            h = self.healths[target[0]]
            h[0] = max(h[0] - amount, 0)
            self.log.append(f"{self.name_of(attacker)} {label(attacker)} hits {self.name_of(target)} {label(target)} for {amount} ({h[0]} left)")
            if h[0] == 0:
                self.log.append(f"{self.name_of(target)} {label(target)} is destroyed")
                self.despawn(target)

    def team_strength(self, team):
        total = 0
        for e in self.living():
            if self.slots[e[0]]["team"] == team and self.healths[e[0]] is not None:
                total += self.healths[e[0]][0]
        return total

    def render(self, tick):
        print(f"=== tick {tick} ===")
        if not self.log:
            print("  (no events)")
        for line in self.log:
            print(f"  * {line}")
        self.log = []
        for i, s in enumerate(self.slots):
            if not s["alive"]:
                print(f"  [{i}] free (last gen {s['generation']})")
                continue
            e = (i, s["generation"])
            p = self.positions[i]
            parts = f"  [{i}] {label(e)} {s['team']} {s['name']} at ({p[0]},{p[1]})"
            h = self.healths[i]
            if h is not None:
                parts += f" hp {h[0]}/{h[1]}"
            m = self.motions[i]
            if m is not None:
                parts += f" vel ({m['vel'][0]},{m['vel'][1]})"
            t = self.targets[i]
            if t is not None:
                parts += f" -> {label(t)}"
            print(parts)
        print(f"  strength: red {self.team_strength('red')} blue {self.team_strength('blue')}")


def main():
    orders = []
    for line in ROSTER:
        try:
            orders.append(parse_order(line))
        except SetupError as err:
            print(f"roster: skipped: {err.describe()}")
    print(f"roster: {len(orders)} spawn orders accepted")
    world = World()
    for tick in range(0, TICKS + 1):
        for (t, team, kind, pos) in orders:
            if t == tick:
                e = world.spawn(kind, team, pos)
                world.log.append(f"spawned {kind} {label(e)} for {team} at ({pos[0]},{pos[1]})")
        if tick > 0:
            world.run_targeting()
            world.run_movement()
            world.run_combat()
        world.render(tick)
    red = world.team_strength("red")
    blue = world.team_strength("blue")
    if red > blue:
        print(f"result: red leads {red} to {blue}")
    elif blue > red:
        print(f"result: blue leads {blue} to {red}")
    else:
        print(f"result: draw at {red}")


main()
