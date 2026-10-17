#!/usr/bin/env python3
"""Reject private data in committed 027 fixtures and evidence (research R13/R15).

Scans ``contracts/fixtures/`` and ``specs/027-rust-server/evidence/`` for
emails, invite codes, session/provider credentials, local paths and content
hashes. Evidence may carry correlation IDs, timings, counts, git SHAs (40 hex)
and screenshots; it must not carry anything that identifies a real person or
fingerprints content. Binary files (screenshots, generated WAV) are skipped.

    python3 scripts/check_evidence_hygiene.py [ROOT ...]

Missing roots are fine (the directories appear as later slices land).
"""

from __future__ import annotations

import math
import re
import sys
from collections import Counter
from pathlib import Path

DEFAULT_ROOTS = ("contracts/fixtures", "specs/027-rust-server/evidence")

RULES: tuple[tuple[str, re.Pattern[str]], ...] = (
    (
        "email",
        # RFC 2606 reserved domains (example.com/.org/.net, *.test, *.invalid)
        # cannot identify a real person, so synthetic addresses on them are fine.
        re.compile(
            r"[A-Za-z0-9._%+-]+@(?!(?:[A-Za-z0-9-]+\.)*(?:example\.(?:com|org|net)"
            r"|[A-Za-z0-9-]+\.(?:test|invalid))(?![A-Za-z0-9-]|\.[A-Za-z0-9-]))"
            r"[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+"
        ),
    ),
    (
        "invite code",
        re.compile(
            r"invite[_ -]?(?:code|token)\"?\s*[:=]\s*\"?[A-Za-z0-9_-]{4,}",
            re.IGNORECASE,
        ),
    ),
    (
        "credential",
        re.compile(
            r"(?:\bsk-[A-Za-z0-9_-]{16,}|\bAKIA[0-9A-Z]{16}\b"
            r"|\bBearer\s+[A-Za-z0-9._~+/=-]{12,}"
            r"|\bFlyV1\s+\S{12,}"
            r"|\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\."
            # Labelled secrets. The value class stops at whitespace, quotes and
            # separators, so a password with punctuation in its first 8
            # characters is still caught; obvious placeholders are exempt.
            r"|[A-Za-z0-9_-]*(?:token|secret|passwd|password|api[_-]?key"
            r"|access[_-]?key(?:[_-]?id)?|cookie|brainbuddy_session)"
            r"\"?\s*[:=]\s*"
            # Quoted values count as written; an unquoted value must carry a
            # digit or symbol so prose such as "a token: cleanTitle" is ignored.
            r"(?:\"(?!<|\$|\{|\*|\[|redacted)[^\s\"',;]{8,}"
            r"|(?!<|\$|\{|\*|\[|redacted)(?=[^\s\"',;]*[^A-Za-z\s\"',;])"
            r"[^\s\"',;]{8,})"
            # Error codes ("validation_error") are not secrets: a ``code`` value
            # must be long and carry a digit or capital to count.
            r"|\bcode\"?\s*[:=]\s*\"?(?-i:(?=[A-Za-z0-9_-]*[0-9A-Z])"
            r"[A-Za-z0-9_-]{20,}))",
            re.IGNORECASE,
        ),
    ),
    (
        "local path",
        re.compile(r"(?:/home/[^/\s\"']+|/Users/[^/\s\"']+|[A-Za-z]:\\Users\\)"),
    ),
    (
        "content hash",
        re.compile(
            r"(?<![0-9A-Fa-f])(?:[0-9A-Fa-f]{64}|[0-9A-Fa-f]{32})(?![0-9A-Fa-f])"
            r"|\b(?:sha256|sha-256|md5|blake3)[:=]\s*\S+",
            re.IGNORECASE,
        ),
    ),
)


# Identity digests that are legitimately 64-hex: they name a corpus, run or
# report, derived from synthetic inputs, and are asserted byte-for-byte by the
# conformance tests. Allowed by JSON key name so the hash pattern stays strict
# everywhere else (a 64-hex under any other key, or in prose, still fails).
ALLOWED_HASH_KEYS = frozenset(
    {"corpus_digest", "provider_config_digest", "run_key", "report_id", "title_sha256"}
)
_KEYED_HASH = re.compile(r'"([A-Za-z0-9_]+)"\s*:\s*"[0-9A-Fa-f]{64}"')


def _strip_allowed_hashes(line: str) -> str:
    return _KEYED_HASH.sub(
        lambda m: "" if m.group(1) in ALLOWED_HASH_KEYS else m.group(0), line
    )


# secrets.token_urlsafe(32) (invite codes, session tokens) is 43 base64url
# characters. Anything that long, mixed-case, not pure hex, with few word
# separators and token-like entropy is treated as a secret wherever it appears.
# UUIDs are removed first so ``form_<uuid>`` identifiers do not fire.
_UUID = re.compile(
    r"[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}"
)
_TOKEN_RUN = re.compile(r"(?<![A-Za-z0-9_-])[A-Za-z0-9_-]{32,}(?![A-Za-z0-9_-])")
# 20,000 sampled token_urlsafe(32) values never fell below 4.35 bits/char, while
# word-built identifiers sit near 3.8-4.2.
MIN_TOKEN_ENTROPY = 4.25
MAX_TOKEN_SEPARATORS = 8


def _entropy(value: str) -> float:
    counts = Counter(value)
    return -sum(n / len(value) * math.log2(n / len(value)) for n in counts.values())


def _has_secret_token(line: str) -> bool:
    for run in _TOKEN_RUN.findall(_UUID.sub("-", line)):
        if re.fullmatch(r"[0-9A-Fa-f]+", run):
            continue
        if not (re.search(r"[a-z]", run) and re.search(r"[A-Z]", run)):
            continue
        # Identifiers in snake_case/kebab-case are word-separated; a token is not.
        separators = len(re.findall(r"[_-]", run))
        if separators > MAX_TOKEN_SEPARATORS:
            continue
        # Plain camelCase names (test method names) carry no digit or separator;
        # only about 1 in 7,500 real tokens is that clean.
        if not (separators or re.search(r"[0-9]", run)):
            continue
        if _entropy(run) >= MIN_TOKEN_ENTROPY:
            return True
    return False


def scan_text(text: str) -> list[tuple[int, str]]:
    """Return ``(line number, rule name)`` for every violation in ``text``."""
    found: list[tuple[int, str]] = []
    for number, line in enumerate(text.splitlines(), start=1):
        checked = _strip_allowed_hashes(line)
        for name, pattern in RULES:
            if pattern.search(checked):
                found.append((number, name))
        if _has_secret_token(checked):
            found.append((number, "high-entropy token"))
    return found


def scan_roots(roots: list[Path]) -> list[str]:
    problems: list[str] = []
    for root in roots:
        files = [root] if root.is_file() else sorted(root.rglob("*"))
        for path in files:
            if not path.is_file():
                continue
            try:
                text = path.read_text(encoding="utf-8")
            except (UnicodeDecodeError, OSError):
                continue
            problems.extend(f"{path}:{line}: {name}" for line, name in scan_text(text))
    return problems


def main(argv: list[str]) -> int:
    roots = [Path(a) for a in argv] or [Path(r) for r in DEFAULT_ROOTS]
    problems = scan_roots([r for r in roots if r.exists()])
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        print(f"evidence hygiene: {len(problems)} violation(s)", file=sys.stderr)
        return 1
    print("evidence hygiene: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
