#!/bin/bash

# Configuration - modify these variables for different services
SERVICE_NAME="upload-download"
EXEC_PATH="/etc/upload-download/upload-download-linux-x86_64"
DESCRIPTION="File Upload & Download Service"
# Runtime arguments - add any arguments you want here
EXEC_ARGS=""

WORKING_DIRECTORY=$(dirname "$EXEC_PATH")
SERVICE_FILE="/etc/systemd/system/${SERVICE_NAME}.service"



# Check if the executable file exists
if [ ! -f "$EXEC_PATH" ]; then
    echo "Error: Executable file $EXEC_PATH not found. Please ensure the file is in the correct directory."
    exit 1
fi

# Grant execute permission to the program
echo "Granting execute permission to the program..."
chmod +x "$EXEC_PATH"

# Create the systemd service file
echo "Creating systemd service file..."
cat <<EOF | sudo tee "$SERVICE_FILE" > /dev/null
[Unit]
Description=$DESCRIPTION
After=network.target

[Service]
Type=simple
ExecStart=$EXEC_PATH $EXEC_ARGS
Restart=on-failure
WorkingDirectory=$WORKING_DIRECTORY

[Install]
WantedBy=multi-user.target
EOF

# Reload systemd configuration
sudo systemctl daemon-reload

# Enable service on boot and start it
echo "Starting service and enabling it to run on boot..."
sudo systemctl enable "$SERVICE_NAME"
sudo systemctl start "$SERVICE_NAME"

echo "✓ Service installed and started successfully!"
echo "You can check the status using: sudo systemctl status $SERVICE_NAME"