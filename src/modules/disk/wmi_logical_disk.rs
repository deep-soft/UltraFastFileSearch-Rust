use serde::{Deserialize, Serialize};
use std::fmt;
use wmi::{COMLibrary, WMIConnection};
use crate::modules::errors::UFFSError;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Win32LogicalDisk {
    /// The `Caption` property provides a short textual description of the logical disk.
    Caption: Option<String>,

    /// The `Description` property provides a description of the logical disk.
    Description: Option<String>,

    /// The `DeviceID` property contains the unique identifier for the logical disk.
    DeviceID: String,

    /// The `DriveType` property indicates the type of disk drive (e.g., local disk, network drive).
    DriveType: Option<u32>,

    /// The `FileSystem` property specifies the file system used by the disk (e.g., NTFS, FAT32).
    FileSystem: Option<String>,

    /// The `FreeSpace` property indicates the available free space on the logical disk, in bytes.
    FreeSpace: Option<u64>,

    /// The `Size` property indicates the total size of the logical disk, in bytes.
    Size: Option<u64>,

    /// The `VolumeName` property contains the label of the volume.
    VolumeName: Option<String>,

    /// The `VolumeSerialNumber` property contains the serial number of the volume.
    VolumeSerialNumber: Option<String>,

    /// The `__CLASS` property specifies the WMI class of the object.
    __CLASS: Option<String>,

    /// The `__DERIVATION` property contains the inheritance hierarchy of the WMI class.
    __DERIVATION: Option<Vec<String>>,

    /// The `__DYNASTY` property specifies the root class in the inheritance hierarchy.
    __DYNASTY: Option<String>,

    /// The `__GENUS` property is an internal classification value used by WMI.
    __GENUS: Option<i32>,

    /// The `__NAMESPACE` property specifies the WMI namespace where the object resides.
    __NAMESPACE: Option<String>,

    /// The `__PATH` property contains the full WMI path to the object.
    __PATH: Option<String>,

    /// The `__PROPERTY_COUNT` property indicates the number of properties in the object.
    __PROPERTY_COUNT: Option<i32>,

    /// The `__RELPATH` property specifies the relative path to the object within the WMI namespace.
    __RELPATH: Option<String>,

    /// The `__SERVER` property specifies the name of the server where the WMI object resides.
    __SERVER: Option<String>,

    /// The `__SUPERCLASS` property contains the immediate superclass of the WMI class.
    __SUPERCLASS: Option<String>,
}

// Implement Display for a nicer output format
impl fmt::Display for Win32LogicalDisk {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if let Some(caption) = &self.Caption {
            write!(f, "Caption                      : {}\n", caption)?;
        }
        if let Some(description) = &self.Description {
            write!(f, "Description                  : {}\n", description)?;
        }
        write!(f, "DeviceID                     : {}\n", self.DeviceID)?;
        if let Some(drive_type) = &self.DriveType {
            write!(f, "DriveType                    : {}\n", drive_type)?;
        }
        if let Some(file_system) = &self.FileSystem {
            write!(f, "FileSystem                   : {}\n", file_system)?;
        }
        if let Some(free_space) = &self.FreeSpace {
            write!(f, "FreeSpace                    : {}\n", free_space)?;
        }
        if let Some(size) = &self.Size {
            write!(f, "Size                         : {}\n", size)?;
        }
        if let Some(volume_name) = &self.VolumeName {
            write!(f, "VolumeName                   : {}\n", volume_name)?;
        }
        if let Some(volume_serial_number) = &self.VolumeSerialNumber {
            write!(f, "VolumeSerialNumber           : {}\n", volume_serial_number)?;
        }
        if let Some(class) = &self.__CLASS {
            write!(f, "__CLASS                      : {}\n", class)?;
        }
        if let Some(derivation) = &self.__DERIVATION {
            write!(f, "__DERIVATION                 : {:?}\n", derivation)?;
        }
        if let Some(dynasty) = &self.__DYNASTY {
            write!(f, "__DYNASTY                    : {}\n", dynasty)?;
        }
        if let Some(genus) = &self.__GENUS {
            write!(f, "__GENUS                      : {}\n", genus)?;
        }
        if let Some(namespace) = &self.__NAMESPACE {
            write!(f, "__NAMESPACE                  : {}\n", namespace)?;
        }
        if let Some(path) = &self.__PATH {
            write!(f, "__PATH                       : {}\n", path)?;
        }
        if let Some(property_count) = &self.__PROPERTY_COUNT {
            write!(f, "__PROPERTY_COUNT             : {}\n", property_count)?;
        }
        if let Some(relpath) = &self.__RELPATH {
            write!(f, "__RELPATH                    : {}\n", relpath)?;
        }
        if let Some(server) = &self.__SERVER {
            write!(f, "__SERVER                     : {}\n", server)?;
        }
        if let Some(superclass) = &self.__SUPERCLASS {
            write!(f, "__SUPERCLASS                 : {}\n", superclass)?;
        }
        Ok(())
    }
}

pub fn query_logical_disk() -> Result<Vec<Win32LogicalDisk>, UFFSError> {
    // Initialize COM library
    let com_con = COMLibrary::new().map_err(|e| UFFSError::WMIQueryFailed(format!("Failed to initialize COM: {:?}", e)))?;

    // Establish a connection to WMI in the correct namespace
    let wmi_con = WMIConnection::with_namespace_path("ROOT\\CIMv2", com_con.into())
        .map_err(|e| UFFSError::WMIQueryFailed(format!("Failed to connect to WMI namespace: {:?}", e)))?;

    // Define the WMI query for logical disks
    let query = "SELECT * FROM Win32_LogicalDisk";

    // Execute the query and get results
    let results: Vec<Win32LogicalDisk> = wmi_con.raw_query(query)
        .map_err(|e| UFFSError::WMIQueryFailed(format!("Failed to execute WMI query: {:?}", e)))?;

    // Check if results are empty
    if results.is_empty() {
        return Err(UFFSError::EmptyDriveInfo);
    }

    // Return the results
    Ok(results)
}
