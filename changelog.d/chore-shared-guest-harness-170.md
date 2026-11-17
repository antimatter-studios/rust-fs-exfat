### Changed

- Linux oracle tiers use fs-linux-test-harness v0.7.0's shared guest without an opt-in directory export; ending a tier or fixture build leaves the guest running, and fixture and `test:vm` timeouts reap unfinished boots without stopping other consumers. CI jobs that boot the guest stop it with `chore vm:down` before checking that it is down.
