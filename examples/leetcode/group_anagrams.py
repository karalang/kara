# examples/leetcode/group_anagrams.py
#
# LeetCode #49: Group Anagrams (Python reference implementation)
# Same logic as group_anagrams.kara

from collections import defaultdict

def group_anagrams(strs):
    groups = defaultdict(list)

    for s in strs:
        key = "".join(sorted(s))
        groups[key].append(s)

    return list(groups.values())


# Test 1
strs = ["eat", "tea", "tan", "ate", "nat", "bat"]
print(f"Input:  {strs}")
print(f"Output: {group_anagrams(strs)}")
print()

# Test 2
strs2 = ["a"]
print(f"Input:  {strs2}")
print(f"Output: {group_anagrams(strs2)}")
print()

# Test 3
strs3 = [""]
print(f"Input:  {strs3}")
print(f"Output: {group_anagrams(strs3)}")
