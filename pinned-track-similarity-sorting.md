# Pinned-track similarity sorting

When a track is pinned, `workspace::pinned_track_sort` orders audio entries by a
lexicographic distance key. The key begins with the tier number, so a difference
in a higher tier always outweighs every possible difference in lower tiers:

1. LoRA differences: differing strength count, total strength magnitude, order
   differences, then identity/count differences.
2. Generation parameters: absolute seed distance, absolute BPM distance, then
   musical key distance around the 24-position major/minor circle.
3. Content: character edit distance for lyrics, then character edit distance for
   prompt.

Workflow files are parsed once per entry during a sort. A missing or invalid
workflow receives the maximum tier and is sorted by filename. The pinned track
is assigned the zero-distance key and therefore remains first. Equal keys use
case-insensitive filename order followed by the original filename as a stable
tie-breaker.

Without a pinned track, the existing alphabetical or modified-date sort mode is
preserved.