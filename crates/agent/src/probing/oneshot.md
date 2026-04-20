# One-shot prober

Trippy-backed campaign prober. Per-pair blocking tracer tasks run under
an independent semaphore so campaign traffic cannot starve continuous MTR
and vice versa. Loss semantics, MTR aggregation, and the shared-resource
audit are filled in as the implementation lands; this page is the
canonical engineer-facing reference once complete.
