# while loop with [ ] test and $(( )) arithmetic
i=0
s=0
while [ $i -lt 50000 ]; do
  s=$((s + i % 7))
  i=$((i + 1))
done
echo $s
