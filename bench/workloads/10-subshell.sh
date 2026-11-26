# subshells
i=0
while [ $i -lt 1000 ]; do
  (x=$i)
  i=$((i + 1))
done
echo done
