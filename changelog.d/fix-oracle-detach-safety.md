### Fixed
- The macOS scale oracle retries only ordinary, receipt-owned detaches and retains its image and receipt when the bounded retry window expires.
- Exhausted shell-runner detaches report process holders of only the current receipt's block and raw nodes, preserving failure and image evidence even when holder lookup fails.
