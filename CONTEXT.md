# Domain context

## Package document

An in-memory Unreal package together with its wire provenance. A package
document contains typed header/table/property values while retaining the exact
encoded bytes needed to reproduce data that was not edited or is not yet
understood.

## Wire provenance

The original binary representation of a decoded value: compact-integer bytes,
property tag bytes, string encoding, object data spans, padding, and unknown
payloads. Decoding must not discard wire provenance. Encoding an unchanged
package document must reproduce the input bytes exactly.

## Semantic edit

A deliberate change to a typed value. A semantic edit invalidates only that
value's wire provenance. Its containing length and offset fields may then be
recomputed by the package codec; unrelated provenance remains unchanged.

## Object reference

A validated reference to no object, an import-table entry, or an export-table
entry. Its signed one-based integer is a wire encoding, not its domain type.

## Package version

The archive and licensee versions that select binary layout rules. Version
checks belong to the package-version or native-object modules rather than
callers and output adapters.

## Native object registry

The class-aware decoder for binary data following an object's tagged
properties. Unknown classes consume nothing, preserving their payload as wire
provenance.
