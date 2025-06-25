#!/bin/bash

# Script to run prove_block for block numbers 0 to 44
# Usage: ./run_prove_blocks.sh

# RPC provider URL
RPC_URL="http://localhost:3000"

# Initialize arrays to store successful and failed blocks
successful_blocks=()
failed_blocks=()

# Loop through block numbers 0 to 170
for block_num in {0..170}
do
    echo "Processing block number: $block_num"
    cargo run -p prove_block --release -- --block-number $block_num --rpc-provider $RPC_URL

    # Check if the command was successful
    if [ $? -eq 0 ]; then
        echo "Successfully processed block $block_num"
        successful_blocks+=($block_num)
    else
        echo "Error processing block $block_num"
        failed_blocks+=($block_num)
    fi

    echo "----------------------------------------"
done

echo "Summary:"
echo "----------------------------------------"
echo "Successful blocks (${#successful_blocks[@]}):"
printf "%s " "${successful_blocks[@]}"
echo -e "\n----------------------------------------"
echo "Failed blocks (${#failed_blocks[@]}):"
printf "%s " "${failed_blocks[@]}"
echo -e "\n----------------------------------------"

# Save failed blocks to a file for easy retry
if [ ${#failed_blocks[@]} -gt 0 ]; then
    echo "Failed blocks have been saved to failed_blocks.txt"
    printf "%s\n" "${failed_blocks[@]}" > failed_blocks.txt
fi
