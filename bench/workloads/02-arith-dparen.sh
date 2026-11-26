# (( )) arithmetic loop
i=0
s=0
while (( i < 100000 )); do
  (( s += i * 3 % 11 ))
  (( i++ ))
done
echo $s
