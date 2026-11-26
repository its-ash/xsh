# pathname expansion
n=0
i=0
while [ $i -lt 50 ]; do
  for f in /usr/bin/*; do
    n=$((n + 1))
  done
  i=$((i + 1))
done
[ $n -gt 0 ] && echo ok
