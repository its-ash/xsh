# parameter expansion: length, prefix/suffix strip, substitution, substring
s=hello-world-from-the-shell
n=0
i=0
while [ $i -lt 20000 ]; do
  a=${s%-*}
  b=${s#*-}
  c=${s/world/xsh}
  d=${s:6:5}
  n=$((n + ${#a} + ${#b} + ${#c} + ${#d}))
  i=$((i + 1))
done
echo $n
