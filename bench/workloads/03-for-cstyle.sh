# C-style for loop
s=0
for ((i = 0; i < 100000; i++)); do
  s=$((s ^ i))
done
echo $s
