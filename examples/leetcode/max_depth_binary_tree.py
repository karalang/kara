# examples/leetcode/max_depth_binary_tree.py
#
# LeetCode #104: Maximum Depth of Binary Tree (Python reference implementation)
# Same logic as max_depth_binary_tree.kara

class TreeNode:
    def __init__(self, val, left=None, right=None):
        self.val   = val
        self.left  = left
        self.right = right


def max_depth(root):
    if root is None:
        return 0
    return 1 + max(max_depth(root.left), max_depth(root.right))


# Test 1: [3,9,20,null,null,15,7] → depth 3
#        3
#       / \
#      9  20
#        /  \
#       15   7
tree1 = TreeNode(3,
    left=TreeNode(9),
    right=TreeNode(20,
        left=TreeNode(15),
        right=TreeNode(7),
    ),
)
print(f"Max depth: {max_depth(tree1)}")

# Test 2: [1,null,2] → depth 2
#   1
#    \
#     2
tree2 = TreeNode(1, right=TreeNode(2))
print(f"Max depth: {max_depth(tree2)}")

# Test 3: empty tree → depth 0
print(f"Max depth: {max_depth(None)}")

# Expected output:
# Max depth: 3
# Max depth: 2
# Max depth: 0
