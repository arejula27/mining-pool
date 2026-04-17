#!/usr/bin/env python3
"""Convert a 32-byte hex string to base58 (no checksum)."""
import sys

ALPHA = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"

n = int(sys.argv[1], 16)
result = ""
while n:
    n, rem = divmod(n, 58)
    result = ALPHA[rem] + result
print(result)
