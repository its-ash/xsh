# read a file line by line
f=${TMPDIR:-/tmp}/xsh-bench-read.$$
i=0
while [ $i -lt 5000 ]; do
  echo "row $i" >> "$f"
  i=$((i + 1))
done
n=0
while IFS= read -r line; do
  n=$((n + ${#line}))
done < "$f"
rm -f "$f"
echo $n
