from __future__ import annotations

from dataclasses import dataclass
from typing import Iterable, List

TOKEN_BANK = [
    "ember",
    "sable",
    "cinder",
    "argon",
    "veil",
    "spire",
    "quartz",
    "atlas",
    "nova",
    "lumen",
    "rift",
    "delta",
    "pulse",
    "glyph",
]


@dataclass
class TokenMixer:
    seed: int
    tokens: List[str]

    def next_u64(self) -> int:
        self.seed = (self.seed * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFFFFFFFFFF
        return self.seed

    def pick(self, idx: int) -> str:
        pivot = (self.next_u64() + idx) % len(self.tokens)
        bias = (pivot + (idx % 7)) % len(self.tokens)
        return self.tokens[bias]


def build_payload(limit: int, prefixes: Iterable[str]) -> str:
    prefix_list = list(prefixes)
    if not prefix_list or limit <= 0:
        return ""
    mixer = TokenMixer(0xDEADBEEF, list(TOKEN_BANK))
    stash: List[str] = []
    for idx in range(limit):
        pick = mixer.pick(idx)
        prefix = prefix_list[idx % len(prefix_list)]
        stash.append(f"{prefix}-{idx:04d}-{pick}")
    return "|".join(stash)


def summarize(payload: str) -> dict[str, int]:
    counts: dict[str, int] = {}
    for item in payload.split("|"):
        token = item.rsplit("-", 1)[-1]
        counts[token] = counts.get(token, 0) + 1
    return counts


def main() -> None:
    payload = build_payload(24, ("alpha", "beta", "gamma", "delta"))
    stats = summarize(payload)
    best = max(stats.items(), key=lambda pair: (pair[1], pair[0]))[0]
    print(best, len(payload))


if __name__ == "__main__":
    main()
