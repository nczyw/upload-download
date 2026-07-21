#!/bin/bash

# Configuration - modify these variables for different services
SERVICE_NAME="upload-download"
SERVICE_FILE="/etc/systemd/system/${SERVICE_NAME}.service"

# Check if the service file exists
if [ ! -f "$SERVICE_FILE" ]; then
    echo "Error: Service file $SERVICE_FILE not found. The service may not be installed."
    exit 1
fi

# Stop the service if it's running
echo "Stopping service..."
sudo systemctl stop "$SERVICE_NAME" 2>/dev/null || true

# Disable the service from starting on boot
echo "Disabling service from boot..."
sudo systemctl disable "$SERVICE_NAME" 2>/dev/null || true

# Remove the service file
echo "Removing service file..."
sudo rm -f "$SERVICE_FILE"

# Reload systemd configuration
echo "Reloading systemd configuration..."
sudo systemctl daemon-reload

echo "✓ Service uninstalled successfully!"