#!/bin/sh

# Script to run prove_block for block numbers 0 to 50
# Usage: ./run_prove_blocks.sh

# RPC provider URL
RPC_URL="http://localhost:8000"

# Initialize counters and temporary files for tracking
successful_count=0
failed_count=0
successful_blocks_file=$(mktemp)
failed_blocks_file=$(mktemp)

# Loop through block numbers 0 to 50
block_num=0
while [ $block_num -le 300 ]
do
    echo "Processing block number: $block_num"
    cargo run -p prove_block --release -- --block-number $block_num --rpc-provider $RPC_URL

    # Check if the command was successful
    if [ $? -eq 0 ]; then
        echo "Successfully processed block $block_num"
        echo $block_num >> "$successful_blocks_file"
        successful_count=$((successful_count + 1))
    else
        echo "Error processing block $block_num"
        echo $block_num >> "$failed_blocks_file"
        failed_count=$((failed_count + 1))
    fi

    echo "----------------------------------------"
    block_num=$((block_num + 1))
done

echo "Summary:"
echo "----------------------------------------"
echo "Successful blocks ($successful_count):"
if [ $successful_count -gt 0 ]; then
    tr '\n' ' ' < "$successful_blocks_file"
    echo
fi
echo "----------------------------------------"
echo "Failed blocks ($failed_count):"
if [ $failed_count -gt 0 ]; then
    tr '\n' ' ' < "$failed_blocks_file"
    echo
fi
echo "----------------------------------------"

# Save failed blocks to a file for easy retry
if [ $failed_count -gt 0 ]; then
    echo "Failed blocks have been saved to failed_blocks.txt"
    cp "$failed_blocks_file" failed_blocks.txt
fi

# Clean up temporary files
rm "$successful_blocks_file" "$failed_blocks_file"
