import './style.scss';

import { zodResolver } from '@hookform/resolvers/zod';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { isUndefined } from 'lodash-es';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { SubmitHandler, useForm } from 'react-hook-form';
import { useNavigate } from 'react-router';
import { z } from 'zod';
import { shallow } from 'zustand/shallow';

import { useI18nContext } from '../../../../i18n/i18n-react';
import { FormInput } from '../../../../shared/defguard-ui/components/Form/FormInput/FormInput';
import { FormSelect } from '../../../../shared/defguard-ui/components/Form/FormSelect/FormSelect';
import { Button } from '../../../../shared/defguard-ui/components/Layout/Button/Button';
import {
  ButtonSize,
  ButtonStyleVariant,
} from '../../../../shared/defguard-ui/components/Layout/Button/types';
import { Card } from '../../../../shared/defguard-ui/components/Layout/Card/Card';
import { MessageBox } from '../../../../shared/defguard-ui/components/Layout/MessageBox/MessageBox';
import { SelectOption } from '../../../../shared/defguard-ui/components/Layout/Select/types';
import useApi from '../../../../shared/hooks/useApi';
import { useToaster } from '../../../../shared/hooks/useToaster';
import { MutationKeys } from '../../../../shared/mutations';
import { QueryKeys } from '../../../../shared/queries';
import { ImportNetworkRequest } from '../../../../shared/types';
import { invalidateMultipleQueries } from '../../../../shared/utils/invalidateMultipleQueries';
import { titleCase } from '../../../../shared/utils/titleCase';
import { validateIpOrDomain } from '../../../../shared/validators';
import { useWizardStore } from '../../hooks/useWizardStore';

interface FormInputs extends Omit<ImportNetworkRequest, 'allowed_groups'> {
  fileName: string;
  allowed_groups: string[];
}
const defaultValues: FormInputs = {
  name: '',
  endpoint: '',
  fileName: '',
  config: '',
  allowed_groups: [],
};
export const WizardNetworkImport = () => {
  const submitRef = useRef<HTMLInputElement>(null);
  const queryClient = useQueryClient();
  const { LL } = useI18nContext();
  const navigate = useNavigate();
  const {
    network: { importNetwork },
    groups: { getGroups },
  } = useApi();
  const toaster = useToaster();
  const [setWizardState, nextStepSubject, submitSubject, resetWizard] = useWizardStore(
    (state) => [
      state.setState,
      state.nextStepSubject,
      state.submitSubject,
      state.resetState,
    ],
    shallow,
  );
  const [groupOptions, setGroupOptions] = useState<SelectOption<string>[]>([]);

  const zodSchema = useMemo(
    () =>
      z.object({
        name: z.string().min(1, LL.form.error.required()),
        endpoint: z
          .string()
          .min(1, LL.form.error.required())
          .refine((val) => validateIpOrDomain(val), LL.form.error.endpoint()),
        fileName: z.string().min(1, LL.form.error.required()),
        config: z.string().min(1, LL.form.error.required()),
        allowed_groups: z.array(z.string().min(1, LL.form.error.minimumLength())),
      }),
    [LL.form.error],
  );

  const { control, handleSubmit, setValue, setError, resetField } = useForm<FormInputs>({
    defaultValues,
    mode: 'all',
    reValidateMode: 'onChange',
    resolver: zodResolver(zodSchema),
  });

  const {
    mutate: importNetworkMutation,
    isPending,
    data,
  } = useMutation({
    mutationFn: importNetwork,
    mutationKey: [MutationKeys.IMPORT_NETWORK],
    onSuccess: (response) => {
      toaster.success(LL.networkConfiguration.form.messages.networkCreated());
      // complete wizard if there is no devices to map
      if (response.devices.length === 0) {
        toaster.success(LL.wizard.completed());
        resetWizard();
        invalidateMultipleQueries(queryClient, [
          [QueryKeys.FETCH_NETWORKS],
          [QueryKeys.FETCH_APP_INFO],
        ]);
        navigate('/admin/overview', { replace: true });
      } else {
        setWizardState({
          importedNetworkDevices: response.devices,
          importedNetworkConfig: response.network,
          loading: false,
        });
        nextStepSubject.next();
      }
    },
    onError: (err) => {
      setWizardState({ loading: false });
      toaster.error(LL.messages.error());
      resetField('fileName');
      resetField('config');
      console.error(err);
    },
  });

  const onValidSubmit: SubmitHandler<FormInputs> = useCallback(
    (data) => {
      if (!isPending) {
        setWizardState({ loading: true });
        importNetworkMutation(data);
      }
    },
    [importNetworkMutation, isPending, setWizardState],
  );

  const handleConfigUpload = () => {
    const input = document.createElement('input');
    input.type = 'file';
    input.multiple = false;
    input.style.display = 'none';
    input.onchange = () => {
      if (input.files && input.files.length === 1) {
        const reader = new FileReader();
        reader.onload = () => {
          if (reader.result && input.files) {
            const res = reader.result;
            setValue('config', res as string);
            setValue('fileName', input.files[0].name);
          }
        };
        reader.onerror = () => {
          toaster.error('Error while reading file.');
          setError('fileName', {
            message: 'Please try again',
          });
        };
        reader.readAsText(input.files[0]);
      }
    };
    input.click();
  };

  useEffect(() => {
    const sub = submitSubject.subscribe(() => {
      submitRef.current?.click();
    });
    return () => sub?.unsubscribe();
  }, [submitSubject]);

  const {
    isLoading: groupsLoading,
    data: fetchGroupsData,
    error: fetchGroupsError,
  } = useQuery({
    queryKey: [QueryKeys.FETCH_GROUPS],
    queryFn: getGroups,
  });

  useEffect(() => {
    if (fetchGroupsError) {
      toaster.error(LL.messages.error());
      console.error(fetchGroupsError);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fetchGroupsError]);

  useEffect(() => {
    if (fetchGroupsData) {
      setGroupOptions(
        fetchGroupsData.groups.map((g) => ({
          key: g,
          value: g,
          label: titleCase(g),
        })),
      );
    }
  }, [fetchGroupsData]);

  return (
    <Card id="wizard-network-import" shaded>
      <form onSubmit={handleSubmit(onValidSubmit)}>
        <FormInput
          controller={{ control, name: 'name' }}
          label={LL.networkConfiguration.form.fields.name.label()}
          disabled={!isUndefined(data)}
        />
        <MessageBox>
          <p>{LL.networkConfiguration.form.helpers.gateway()}</p>
        </MessageBox>
        <FormInput
          controller={{ control, name: 'endpoint' }}
          label={LL.networkConfiguration.form.fields.endpoint.label()}
          disabled={!isUndefined(data)}
        />
        <MessageBox>
          <p>{LL.networkConfiguration.form.helpers.allowedGroups()}</p>
        </MessageBox>
        <FormSelect
          controller={{ control, name: 'allowed_groups' }}
          label={LL.networkConfiguration.form.fields.allowedGroups.label()}
          loading={groupsLoading}
          disabled={!isUndefined(data)}
          options={groupOptions}
          placeholder={LL.networkConfiguration.form.fields.allowedGroups.placeholder()}
          renderSelected={(group) => ({
            key: group,
            displayValue: titleCase(group),
          })}
        />
        <FormInput
          controller={{ control, name: 'fileName' }}
          label={LL.wizard.locations.form.fileName()}
          disabled
        />
        <Button
          text={LL.wizard.locations.form.selectFile()}
          size={ButtonSize.SMALL}
          styleVariant={ButtonStyleVariant.STANDARD}
          onClick={() => handleConfigUpload()}
          className="upload"
          data-testid="upload-config"
        />
        <input className="visually-hidden" type="submit" ref={submitRef} />
      </form>
    </Card>
  );
};
