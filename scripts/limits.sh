#!/usr/bin/env bash
# Size limits for the source tree.
#
# Long files and long functions are the two things that make a codebase
# unreadable without ever failing a compiler or a lint, so they are checked
# here and enforced by the build gate rather than left to review.
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MAX_FILE_LINES="${LCC_MAX_FILE_LINES:-1000}"
MAX_FN_LINES="${LCC_MAX_FN_LINES:-100}"

cd "$PROJECT_DIR"
mapfile -t FILES < <(git ls-files '*.rs')

failures=0

for file in "${FILES[@]}"; do
    lines=$(wc -l <"$file")
    if (( lines > MAX_FILE_LINES )); then
        echo "$file: $lines lines, limit $MAX_FILE_LINES" >&2
        failures=1
    fi
done

# Function length, counted from the line declaring it to the line closing its
# body. Braces inside strings, chars and comments would confuse a brace count,
# so those are blanked out first.
for file in "${FILES[@]}"; do
    awk -v max="$MAX_FN_LINES" -v file="$file" '
    {
        line = $0
        # Blank out what must not be counted: line comments, string and char
        # literals. Crude, but it only has to preserve brace balance.
        gsub(/\\"/, "", line)
        gsub(/"[^"]*"/, "\"\"", line)
        gsub(/'\''\{'\''|'\''\}'\''/, "", line)
        sub(/\/\/.*/, "", line)

        if (depth == 0) {
            # A function signature may span several lines, so the body is taken
            # to start at the first "{" at or after the "fn".
            if (line ~ /(^|[^[:alnum:]_])fn[[:space:]]+/) { pending = 1; start = NR; name = line }
        }

        if (pending || depth > 0) {
            opens = gsub(/\{/, "{", line)
            closes = gsub(/\}/, "}", line)
            if (pending && opens > 0) { pending = 0; depth = 0 }
            depth += opens - closes
            if (depth <= 0 && !pending) {
                length_ = NR - start + 1
                if (length_ > max) {
                    sub(/^[[:space:]]*/, "", name)
                    printf "%s:%d: function is %d lines, limit %d\n    %s\n", file, start, length_, max, name > "/dev/stderr"
                    bad = 1
                }
                depth = 0
            }
        }
    }
    END { exit bad ? 1 : 0 }
    ' "$file" || failures=1
done

if (( failures )); then
    echo "==> size limits exceeded" >&2
    exit 1
fi
echo "==> size limits: ${#FILES[@]} files within $MAX_FILE_LINES lines, functions within $MAX_FN_LINES"
