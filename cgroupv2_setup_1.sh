#!/bin/sh

set -e

for i in $(seq 1 10)
do
  cgcreate -g memory,cpu:workerd1_$i
  echo 100M > /sys/fs/cgroup/workerd1_$i/memory.max
  echo "50000 1000000" > /sys/fs/cgroup/workerd1_$i/cpu.max
done
