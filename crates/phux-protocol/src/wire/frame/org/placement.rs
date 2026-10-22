//! The bounded positional creation-placement value inside an org TLV field.

use super::{
    ActivationId, CreatePlacement, DecodeError, Decoder, Encoder, ResourceKey, ServerInstance,
    SplitDirection,
};

pub(super) fn encode(value: &CreatePlacement, enc: &mut Encoder<'_>) {
    match value {
        CreatePlacement::NewGroup => enc.write_u8(0),
        CreatePlacement::Beside {
            entry,
            resource,
            instance,
            activation,
            dir,
            ratio,
        } => {
            enc.write_u8(1);
            for byte in entry
                .iter()
                .chain(resource.as_bytes())
                .chain(instance.as_bytes())
            {
                enc.write_u8(*byte);
            }
            enc.write_u64_be(activation.get());
            enc.write_u8(dir.to_u8());
            enc.write_f32_be(*ratio);
        }
    }
}

pub(super) fn decode(bytes: &[u8]) -> Result<CreatePlacement, DecodeError> {
    let mut dec = Decoder::new(bytes);
    match dec.read_u8()? {
        0 => Ok(CreatePlacement::NewGroup),
        1 => {
            let entry = read16(&mut dec)?;
            let resource =
                ResourceKey::new(read16(&mut dec)?).ok_or(DecodeError::InvalidOrganizationKey)?;
            let instance = ServerInstance::new(read16(&mut dec)?);
            let activation = ActivationId::new(dec.read_u64_be()?);
            let raw = dec.read_u8()?;
            let dir = SplitDirection::from_u8(raw)
                .ok_or_else(|| DecodeError::unknown_enum("SplitDirection", raw))?;
            let ratio = dec.read_f32_be()?;
            if !ratio.is_finite() || ratio <= 0.0 || ratio >= 1.0 {
                return Err(DecodeError::InvalidArrangement);
            }
            Ok(CreatePlacement::Beside {
                entry,
                resource,
                instance,
                activation,
                dir,
                ratio,
            })
        }
        other => Err(DecodeError::unknown_enum("CreatePlacement", other)),
    }
}

fn read16(dec: &mut Decoder<'_>) -> Result<[u8; 16], DecodeError> {
    let mut bytes = [0; 16];
    for byte in &mut bytes {
        *byte = dec.read_u8()?;
    }
    Ok(bytes)
}
