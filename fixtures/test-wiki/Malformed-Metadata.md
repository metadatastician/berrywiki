<!-- cherrywiki
id: 0195f6d0-0000-7000-8000-000000000007
parent: 0195f6d0-0000-7000-8000-000000000001
position: not-a-number
kind: page
archived: maybe
icon: rocket
-->

# Malformed Metadata

This page has an invalid `position` and `archived`, plus an unknown `icon`
field. It must remain readable, emit warnings, default the bad fields, and
preserve the unknown `icon` field on re-serialisation.
