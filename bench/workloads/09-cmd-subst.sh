# command substitution
i=0
t=0
while [ $i -lt 1000 ]; do
  v=$(echo $i)
  t=$((t + v))
  i=$((i + 1))
done
echo $t
