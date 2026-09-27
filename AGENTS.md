# Test policy

Prefer a few real server, protocol, storage and executable scenarios over per-helper unit tests.
Keep focused unit coverage for security, measurement integrity and critical resource ownership.
Use shared Go/Rust contract vectors instead of duplicating their cases in local unit tests.
Do not test constants, copy, trivial helpers, library behaviour or internal call order.
Remove redundant coverage when adding a boundary regression; reuse the existing fixture.
Keep third-party fork suites intact when validating dependency changes.
