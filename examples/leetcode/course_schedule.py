# examples/leetcode/course_schedule.py
#
# LeetCode #207: Course Schedule (Python reference implementation)
# Same logic as course_schedule.kara

from enum import Enum, auto

class VisitState(Enum):
    UNVISITED = auto()
    VISITING  = auto()  # currently on the DFS stack — a back edge here means a cycle
    VISITED   = auto()  # fully processed, no cycle reachable from this node


def can_finish(num_courses, prerequisites):
    # Build adjacency list: prereq → courses that require it
    graph = {i: [] for i in range(num_courses)}
    for course, prereq in prerequisites:
        graph[prereq].append(course)

    state = {i: VisitState.UNVISITED for i in range(num_courses)}

    def has_cycle(node):
        state[node] = VisitState.VISITING
        for neighbor in graph[node]:
            if state[neighbor] == VisitState.VISITING:
                return True   # back edge — cycle found
            if state[neighbor] == VisitState.UNVISITED:
                if has_cycle(neighbor):
                    return True
        state[node] = VisitState.VISITED
        return False

    for i in range(num_courses):
        if state[i] == VisitState.UNVISITED:
            if has_cycle(i):
                return False

    return True


# Test 1: 2 courses, take 0 before 1 → no cycle → True
print(f"Can finish: {can_finish(2, [(1, 0)])}")

# Test 2: 2 courses, 0↔1 mutual dependency → cycle → False
print(f"Can finish: {can_finish(2, [(1, 0), (0, 1)])}")

# Test 3: 4 courses, diamond dependency → no cycle → True
#   0 → 1 → 3
#   0 → 2 → 3
print(f"Can finish: {can_finish(4, [(1, 0), (2, 0), (3, 1), (3, 2)])}")

# Test 4: 3 courses, 0 → 1 → 2 → 0 → cycle → False
print(f"Can finish: {can_finish(3, [(1, 0), (2, 1), (0, 2)])}")

# Expected output:
# Can finish: True
# Can finish: False
# Can finish: True
# Can finish: False
