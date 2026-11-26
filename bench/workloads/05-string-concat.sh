# grow a string by concatenation
s=
i=0
while [ $i -lt 20000 ]; do
  s="$s$i,"
  i=$((i + 1))
done
echo ${#s}
