# examples/leetcode/lru_cache.py
#
# LeetCode #146: LRU Cache (Python reference implementation)
# Same logic as lru_cache.kara

class LruCache:
    def __init__(self, capacity):
        self.capacity = capacity
        self.entries = []            # ordered: most recent at the end
        self.index = {}              # key → position in entries

    def get(self, key):
        if key not in self.index:
            return None

        pos = self.index[key]
        entry = self.entries.pop(pos)

        # Move to end (most recently used)
        self.entries.append(entry)
        self._rebuild_index()

        return entry["value"]

    def put(self, key, value):
        if key in self.index:
            # Remove old entry
            self.entries.pop(self.index[key])
        elif len(self.entries) == self.capacity:
            # Evict least recently used (front)
            self.entries.pop(0)

        # Insert at end (most recently used)
        self.entries.append({"key": key, "value": value})
        self._rebuild_index()

    def _rebuild_index(self):
        self.index = {entry["key"]: i for i, entry in enumerate(self.entries)}


cache = LruCache(2)

cache.put(1, 1)
cache.put(2, 2)
print(f"get(1) = {cache.get(1)}")      # 1

cache.put(3, 3)                         # evicts key 2
print(f"get(2) = {cache.get(2)}")      # None

cache.put(4, 4)                         # evicts key 1
print(f"get(1) = {cache.get(1)}")      # None
print(f"get(3) = {cache.get(3)}")      # 3
print(f"get(4) = {cache.get(4)}")      # 4
