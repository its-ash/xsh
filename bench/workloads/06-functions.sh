# function calls with local variables and arguments
add() {
  local a=$1 b=$2
  r=$((a + b))
}
i=0
t=0
while [ $i -lt 30000 ]; do
  add $i 3
  t=$((t + r))
  i=$((i + 1))
done
echo $t
